use std::time::{Duration, Instant};

use alloy::{
    network::Network,
    providers::{Provider, RootProvider},
    pubsub::Subscription,
    transports::{RpcError, TransportErrorKind},
};
use tokio::sync::broadcast::error::RecvError;
use tracing::{error, info, warn};

use crate::{RobustProvider, robust_provider::Error};

/// Default time interval between primary provider reconnection attempts (30 seconds)
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
    ///
    /// # Errors
    ///
    /// Returns an error if all providers have been exhausted and failed.
    pub async fn recv(&mut self) -> Result<N::HeaderResponse, Error> {
        loop {
            // Check if we should attempt to reconnect to primary
            if self.should_reconnect_to_primary() {
                info!("Attempting to reconnect to primary provider");
                if let Err(e) = self.try_reconnect_to_primary().await {
                    warn!(error = %e, "Failed to reconnect to primary provider");
                } else {
                    info!("Successfully reconnected to primary provider");
                }
            }

            // Try to receive from current subscription
            if let Some(subscription) = &mut self.subscription {
                match subscription.recv().await {
                    Ok(header) => {
                        self.consecutive_lags = 0;
                        return Ok(header);
                    }
                    Err(recv_error) => match recv_error {
                        RecvError::Closed => {
                            error!("Subscription channel closed, switching provider");
                            // NOTE: Not sure what error to pass here
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
                                // NOTE: Not sure what error to pass here
                                let error =
                                    RpcError::Transport(TransportErrorKind::BackendGone).into();
                                self.switch_to_fallback(error).await?;
                            }
                        }
                    },
                }
            } else {
                // No subscription available
                return Err(RpcError::Transport(TransportErrorKind::BackendGone).into());
            }
        }
    }

    /// Check if we should attempt to reconnect to the primary provider
    fn should_reconnect_to_primary(&self) -> bool {
        // Only attempt reconnection if enough time has passed since last attempt
        // The RobustProvider will try the primary provider first automatically
        match self.last_reconnect_attempt {
            None => false,
            Some(last_attempt) => last_attempt.elapsed() >= self.reconnect_interval,
        }
    }

    /// Attempt to reconnect to the primary provider
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

    /// Switch to a fallback provider
    async fn switch_to_fallback(&mut self, last_error: Error) -> Result<(), Error> {
        // Mark that we need reconnection attempts
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
}
