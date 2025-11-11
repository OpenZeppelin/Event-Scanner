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
use tokio_stream::{Stream, wrappers::UnboundedReceiverStream};
use tracing::{error, info, warn};

use crate::{RobustProvider, robust_provider::Error};

/// Default time interval between primary provider reconnection attempts
pub const DEFAULT_RECONNECT_INTERVAL: Duration = Duration::from_secs(30);

/// Maximum number of consecutive lags before switching providers
const MAX_LAG_COUNT: usize = 3;

/// A robust subscription wrapper that automatically handles provider failover
/// and periodic reconnection attempts to the primary provider.
#[derive(Debug)]
pub struct RobustSubscription<N: Network> {
    subscription: Option<Subscription<N::HeaderResponse>>,
    robust_provider: RobustProvider<N>,
    reconnect_interval: Duration,
    last_reconnect_attempt: Option<Instant>,
    consecutive_lags: usize,
}

impl<N: Network> RobustSubscription<N> {
    /// Create a new [`RobustSubscription`]
    pub(crate) fn new(
        subscription: Subscription<N::HeaderResponse>,
        robust_provider: RobustProvider<N>,
        reconnect_interval: Duration,
    ) -> Self {
        Self {
            subscription: Some(subscription),
            robust_provider,
            reconnect_interval,
            last_reconnect_attempt: None,
            consecutive_lags: 0,
        }
    }

    /// Receive the next item from the subscription with automatic failover.
    ///
    /// This method will:
    /// - Attempt to receive from the current subscription
    /// - Handle errors by switching to fallback providers
    /// - Periodically attempt to reconnect to the primary provider
    /// - Will switch to fallback providers if subscription timeout is exhausted
    ///
    /// # Errors
    ///
    /// Returns an error if all providers have been exhausted and failed.
    pub async fn recv(&mut self) -> Result<N::HeaderResponse, Error> {
        loop {
            if self.should_reconnect_to_primary() {
                info!("Attempting to reconnect to primary provider");
                if let Err(e) = self.try_reconnect_to_primary().await {
                    warn!(error = %e, "Failed to reconnect to primary provider");
                } else {
                    info!("Successfully reconnected to primary provider");
                }
            }

            if let Some(subscription) = &mut self.subscription {
                let subscription_timeout = self.robust_provider.subscription_timeout;
                match timeout(subscription_timeout, subscription.recv()).await {
                    Ok(recv_result) => match recv_result {
                        Ok(header) => {
                            self.consecutive_lags = 0;
                            return Ok(header);
                        }
                        Err(recv_error) => match recv_error {
                            RecvError::Closed => {
                                error!("Subscription channel closed, switching provider");
                                let error =
                                    RpcError::Transport(TransportErrorKind::BackendGone).into();
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
                                    let error =
                                        RpcError::Transport(TransportErrorKind::BackendGone).into();
                                    self.switch_to_fallback(error).await?;
                                }
                            }
                        },
                    },
                    Err(e) => {
                        error!(
                            timeout_secs = subscription_timeout.as_secs(),
                            "Subscription timeout - no block received, switching provider"
                        );
                        self.switch_to_fallback(e.into()).await?;
                    }
                }
            } else {
                // No subscription available
                return Err(RpcError::Transport(TransportErrorKind::BackendGone).into());
            }
        }
    }

    fn should_reconnect_to_primary(&self) -> bool {
        // Only attempt reconnection if enough time has passed since last attempt
        match self.last_reconnect_attempt {
            None => false,
            Some(last_attempt) => last_attempt.elapsed() >= self.reconnect_interval,
        }
    }

    async fn try_reconnect_to_primary(&mut self) -> Result<(), Error> {
        self.last_reconnect_attempt = Some(Instant::now());

        let operation =
            move |provider: RootProvider<N>| async move { provider.subscribe_blocks().await };

        let primary = self.robust_provider.primary();
        let subscription =
            self.robust_provider.try_provider_with_timeout(primary, &operation).await;

        if let Err(e) = &subscription {
            error!(error = %e, "eth_subscribe failed");
            return Err(e.clone());
        }

        let subscription = subscription?;
        self.subscription = Some(subscription);
        Ok(())
    }

    async fn switch_to_fallback(&mut self, last_error: Error) -> Result<(), Error> {
        if self.last_reconnect_attempt.is_none() {
            self.last_reconnect_attempt = Some(Instant::now());
        }

        let operation =
            move |provider: RootProvider<N>| async move { provider.subscribe_blocks().await };
        let subscription = self
            .robust_provider
            .try_fallback_providers(
                self.robust_provider.providers.iter().skip(1),
                &operation,
                true,
                last_error,
            )
            .await;

        if let Err(e) = &subscription {
            error!(error = %e, "eth_subscribe failed");
            return Err(e.clone());
        }

        let subscription = subscription?;
        self.subscription = Some(subscription);
        Ok(())
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
        // TODO: This shouldnt be unbounded need choose an appropriate bound (Maybe same as max
        // buffer probably should be bigger)
        let (tx, rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            loop {
                match self.recv().await {
                    Ok(item) => {
                        if tx.send(Ok(item)).is_err() {
                            // Receiver dropped, exit the loop
                            break;
                        }
                    }
                    Err(e) => {
                        // Send the error and exit
                        let _ = tx.send(Err(e));
                        break;
                    }
                }
            }
        });

        RobustSubscriptionStream { inner: UnboundedReceiverStream::new(rx) }
    }
}

/// A stream wrapper around [`RobustSubscription`] that implements the [`Stream`] trait.
pub struct RobustSubscriptionStream<N: Network> {
    inner: UnboundedReceiverStream<Result<N::HeaderResponse, Error>>,
}

impl<N: Network> Stream for RobustSubscriptionStream<N> {
    type Item = Result<N::HeaderResponse, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}
