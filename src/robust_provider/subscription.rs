use std::{
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use alloy::{
    network::Network,
    providers::{Provider, RootProvider},
    pubsub::Subscription,
    transports::{RpcError, TransportErrorKind},
};
use tokio::{
    sync::{broadcast::error::RecvError, mpsc},
    time::timeout,
};
use tokio_stream::{Stream, wrappers::ReceiverStream};
use tracing::{error, info, warn};

use crate::robust_provider::{Error, RobustProvider};

/// Default time interval between primary provider reconnection attempts
pub const DEFAULT_RECONNECT_INTERVAL: Duration = Duration::from_secs(30);

/// Maximum number of consecutive lags before switching providers
const MAX_LAG_COUNT: usize = 3;

/// Max amount of buffered blocks stream can hold
pub const MAX_BUFFERED_BLOCKS: usize = 50000;

/// A robust subscription wrapper that automatically handles provider failover
/// and periodic reconnection attempts to the primary provider.
#[derive(Debug)]
pub struct RobustSubscription<N: Network> {
    subscription: Option<Subscription<N::HeaderResponse>>,
    robust_provider: RobustProvider<N>,
    last_reconnect_attempt: Option<Instant>,
    consecutive_lags: usize,
    current_fallback_index: Option<usize>,
}

impl<N: Network> RobustSubscription<N> {
    /// Create a new [`RobustSubscription`]
    pub(crate) fn new(
        subscription: Subscription<N::HeaderResponse>,
        robust_provider: RobustProvider<N>,
    ) -> Self {
        Self {
            subscription: Some(subscription),
            robust_provider,
            last_reconnect_attempt: None,
            consecutive_lags: 0,
            current_fallback_index: None,
        }
    }

    /// Receive the next item from the subscription with automatic failover.
    ///
    /// This method will:
    /// * Attempt to receive from the current subscription
    /// * Handle errors by switching to fallback providers
    /// * Periodically attempt to reconnect to the primary provider
    /// * Will switch to fallback providers if subscription timeout is exhausted
    ///
    /// # Errors
    ///
    /// Returns an error if all providers have been exhausted and failed.
    pub async fn recv(&mut self) -> Result<N::HeaderResponse, Error> {
        let subscription_timeout = self.robust_provider.subscription_timeout;
        loop {
            self.try_reconnect_to_primary().await;

            if let Some(subscription) = &mut self.subscription {
                let recv_result = timeout(subscription_timeout, subscription.recv()).await;
                match recv_result {
                    Ok(recv_result) => match recv_result {
                        Ok(header) => {
                            self.consecutive_lags = 0;
                            return Ok(header);
                        }
                        Err(recv_error) => {
                            self.process_recv_error(recv_error).await?;
                        }
                    },
                    Err(elapsed_err) => {
                        error!(
                            timeout_secs = subscription_timeout.as_secs(),
                            "Subscription timeout - no block received, switching provider"
                        );

                        self.switch_to_fallback(elapsed_err.into()).await?;
                    }
                }
            } else {
                // No subscription available
                return Err(RpcError::Transport(TransportErrorKind::BackendGone).into());
            }
        }
    }

    /// Process subscription receive errors and handle failover
    async fn process_recv_error(&mut self, recv_error: RecvError) -> Result<(), Error> {
        match recv_error {
            RecvError::Closed => {
                error!("Subscription channel closed, switching provider");
                let error = RpcError::Transport(TransportErrorKind::BackendGone).into();
                self.switch_to_fallback(error).await?;
            }
            RecvError::Lagged(skipped) => {
                self.consecutive_lags += 1;
                warn!(
                    skipped = skipped,
                    consecutive_lags = self.consecutive_lags,
                    "Subscription lagged"
                );

                if self.consecutive_lags >= MAX_LAG_COUNT {
                    error!("Too many consecutive lags, switching provider");
                    let error = RpcError::Transport(TransportErrorKind::BackendGone).into();
                    self.switch_to_fallback(error).await?;
                }
            }
        }
        Ok(())
    }

    /// Try to reconnect to the primary provider if enough time has elapsed.
    /// Returns true if reconnection was successful, false if it's not time yet or if it failed.
    async fn try_reconnect_to_primary(&mut self) -> bool {
        // Check if we should attempt reconnection
        let should_reconnect = match self.last_reconnect_attempt {
            None => false,
            Some(last_attempt) => last_attempt.elapsed() >= self.robust_provider.reconnect_interval,
        };

        if !should_reconnect {
            return false;
        }

        info!("Attempting to reconnect to primary provider");

        self.last_reconnect_attempt = Some(Instant::now());

        let operation =
            move |provider: RootProvider<N>| async move { provider.subscribe_blocks().await };

        let primary = self.robust_provider.primary();
        let subscription =
            self.robust_provider.try_provider_with_timeout(primary, &operation).await;

        match subscription {
            Ok(sub) => {
                info!("Successfully reconnected to primary provider");
                self.subscription = Some(sub);
                self.current_fallback_index = None;
                true
            }
            Err(e) => {
                warn!(error = %e, "Failed to reconnect to primary provider");
                false
            }
        }
    }

    async fn switch_to_fallback(&mut self, last_error: Error) -> Result<(), Error> {
        if self.last_reconnect_attempt.is_none() {
            self.last_reconnect_attempt = Some(Instant::now());
        }

        let operation =
            move |provider: RootProvider<N>| async move { provider.subscribe_blocks().await };

        // Start searching from the next provider after the current one
        let start_index = self.current_fallback_index.map_or(0, |idx| idx + 1);

        let subscription = self
            .robust_provider
            .try_fallback_providers_from(&operation, true, last_error, start_index)
            .await;

        match subscription {
            Ok((sub, fallback_idx)) => {
                self.subscription = Some(sub);
                self.current_fallback_index = Some(fallback_idx);
                Ok(())
            }
            Err(e) => {
                error!(error = %e, "eth_subscribe failed - no fallbacks available");
                Err(e)
            }
        }
    }

    /// Check if the subscription channel is empty (no pending messages)
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match &self.subscription {
            Some(sub) => sub.is_empty(),
            None => true,
        }
    }

    /// Convert the subscription into a stream.
    ///
    /// This spawns a background task that continuously receives from the subscription
    /// and forwards items to a channel, which is then wrapped in a Stream.
    #[must_use]
    pub fn into_stream(mut self) -> RobustSubscriptionStream<N> {
        let (tx, rx) = mpsc::channel(MAX_BUFFERED_BLOCKS);

        tokio::spawn(async move {
            loop {
                match self.recv().await {
                    Ok(item) => {
                        if let Err(err) = tx.send(Ok(item)).await {
                            warn!(error = %err, "Downstream channel closed, stopping stream");
                            break;
                        }
                    }
                    Err(e) => {
                        if let Err(err) = tx.send(Err(e)).await {
                            warn!(error = %err, "Downstream channel closed, stopping stream");
                        }
                        break;
                    }
                }
            }
        });

        RobustSubscriptionStream { inner: ReceiverStream::new(rx) }
    }
}

/// A stream wrapper around [`RobustSubscription`] that implements the [`Stream`] trait.
pub struct RobustSubscriptionStream<N: Network> {
    inner: ReceiverStream<Result<N::HeaderResponse, Error>>,
}

impl<N: Network> Stream for RobustSubscriptionStream<N> {
    type Item = Result<N::HeaderResponse, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}
