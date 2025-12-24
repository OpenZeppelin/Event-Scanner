use std::{pin::Pin, time::Duration};

use alloy::{network::Network, providers::RootProvider};

use crate::robust_provider::{
    IntoRootProvider, RobustProvider, provider::Error, subscription::DEFAULT_RECONNECT_INTERVAL,
};

type BoxedProviderFuture<N> = Pin<Box<dyn Future<Output = Result<RootProvider<N>, Error>> + Send>>;

// RPC retry and timeout settings
/// Default timeout used by `RobustProvider`
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Default timeout for subscriptions
pub const DEFAULT_SUBSCRIPTION_TIMEOUT: Duration = Duration::from_secs(120);
/// Default maximum number of retry attempts.
pub const DEFAULT_MAX_RETRIES: usize = 3;
/// Default base delay between retries.
pub const DEFAULT_MIN_DELAY: Duration = Duration::from_secs(1);
/// Default subscription channel size.
pub const DEFAULT_SUBSCRIPTION_BUFFER_CAPACITY: usize = 128;

/// Builder for constructing a [`RobustProvider`].
///
/// Use this to configure timeouts, retry/backoff, and one or more fallback providers.
pub struct RobustProviderBuilder<N: Network, P: IntoRootProvider<N>> {
    primary_provider: P,
    fallback_providers: Vec<BoxedProviderFuture<N>>,
    call_timeout: Duration,
    subscription_timeout: Duration,
    max_retries: usize,
    min_delay: Duration,
    reconnect_interval: Duration,
    subscription_buffer_capacity: usize,
}

impl<N: Network, P: IntoRootProvider<N>> RobustProviderBuilder<N, P> {
    /// Create a new [`RobustProvider`] with default settings.
    ///
    /// The provided provider is treated as the primary provider.
    /// Any type implementing [`IntoRootProvider`] can be used.
    #[must_use]
    pub fn new(provider: P) -> Self {
        Self {
            primary_provider: provider,
            fallback_providers: vec![],
            call_timeout: DEFAULT_CALL_TIMEOUT,
            subscription_timeout: DEFAULT_SUBSCRIPTION_TIMEOUT,
            max_retries: DEFAULT_MAX_RETRIES,
            min_delay: DEFAULT_MIN_DELAY,
            reconnect_interval: DEFAULT_RECONNECT_INTERVAL,
            subscription_buffer_capacity: DEFAULT_SUBSCRIPTION_BUFFER_CAPACITY,
        }
    }

    /// Create a new [`RobustProvider`] with no retry attempts and only timeout set.
    ///
    /// The provided provider is treated as the primary provider.
    #[must_use]
    pub fn fragile(provider: P) -> Self {
        Self::new(provider).max_retries(0).min_delay(Duration::ZERO)
    }

    /// Add a fallback provider to the list.
    ///
    /// Fallback providers are used when the primary provider times out or fails.
    #[must_use]
    pub fn fallback<F: IntoRootProvider<N> + Send + 'static>(mut self, provider: F) -> Self {
        self.fallback_providers.push(Box::pin(provider.into_root_provider()));
        self
    }

    /// Set the maximum timeout for RPC operations.
    #[must_use]
    pub fn call_timeout(mut self, timeout: Duration) -> Self {
        self.call_timeout = timeout;
        self
    }

    /// Set the timeout for subscription operations.
    ///
    /// This should be set higher than [`call_timeout`](Self::call_timeout) to accommodate chains
    /// with slow block times. Default is [`DEFAULT_SUBSCRIPTION_TIMEOUT`].
    #[must_use]
    pub fn subscription_timeout(mut self, timeout: Duration) -> Self {
        self.subscription_timeout = timeout;
        self
    }

    /// Set the subscription stream buffer capacity.
    ///
    /// Controls the buffer capacity for subscription streams. If new blocks arrive
    /// while the stream buffer is full, a lagged error will be emitted, indicating
    /// that stream items were dropped due to the consumer not keeping pace with the stream.
    ///
    /// Default is [`DEFAULT_SUBSCRIPTION_BUFFER_CAPACITY`].
    #[must_use]
    pub fn subscription_buffer_capacity(mut self, buffer_capacity: usize) -> Self {
        self.subscription_buffer_capacity = buffer_capacity;
        self
    }

    /// Set the maximum number of retry attempts.
    #[must_use]
    pub fn max_retries(mut self, max_retries: usize) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Set the base delay for exponential backoff retries.
    #[must_use]
    pub fn min_delay(mut self, min_delay: Duration) -> Self {
        self.min_delay = min_delay;
        self
    }

    /// Set the interval for attempting to reconnect to the primary provider.
    ///
    /// After a failover to a fallback provider, the subscription will periodically
    /// attempt to reconnect to the primary provider at this interval.
    /// Default is [`DEFAULT_RECONNECT_INTERVAL`].
    #[must_use]
    pub fn reconnect_interval(mut self, reconnect_interval: Duration) -> Self {
        self.reconnect_interval = reconnect_interval;
        self
    }

    /// Build the `RobustProvider`.
    ///
    /// Final builder method: consumes the builder and returns the built [`RobustProvider`].
    ///
    /// # Errors
    ///
    /// Returns an error if any of the providers fail to connect.
    pub async fn build(self) -> Result<RobustProvider<N>, Error> {
        debug!(
            call_timeout_ms = self.call_timeout.as_millis(),
            subscription_timeout_ms = self.subscription_timeout.as_millis(),
            max_retries = self.max_retries,
            fallback_count = self.fallback_providers.len(),
            "Building RobustProvider"
        );

        let primary_provider = self.primary_provider.into_root_provider().await?;

        let mut fallback_providers = Vec::with_capacity(self.fallback_providers.len());
        for (idx, fallback) in self.fallback_providers.into_iter().enumerate() {
            trace!(fallback_index = idx, "Connecting fallback provider");
            fallback_providers.push(fallback.await?);
        }

        info!("RobustProvider initialized");

        Ok(RobustProvider {
            primary_provider,
            fallback_providers,
            call_timeout: self.call_timeout,
            subscription_timeout: self.subscription_timeout,
            max_retries: self.max_retries,
            min_delay: self.min_delay,
            reconnect_interval: self.reconnect_interval,
            subscription_buffer_capacity: self.subscription_buffer_capacity,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::providers::{ProviderBuilder, WsConnect};
    use alloy_node_bindings::Anvil;

    #[tokio::test]
    async fn test_builder_primary_type_different_to_fallback() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn()?;

        let fill_provider = ProviderBuilder::new()
            .connect_ws(WsConnect::new(anvil.ws_endpoint_url().as_str()))
            .await?;

        let root_provider = RootProvider::new_http(anvil.endpoint_url());

        let robust = RobustProviderBuilder::new(fill_provider)
            .fallback(root_provider)
            .call_timeout(Duration::from_secs(5))
            .build()
            .await?;

        assert_eq!(robust.fallback_providers.len(), 1);

        Ok(())
    }

    #[tokio::test]
    async fn test_builder_with_multiple_fallback_types() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn()?;

        let fill_provider = ProviderBuilder::new()
            .connect_ws(WsConnect::new(anvil.ws_endpoint_url().as_str()))
            .await?;

        let root_provider = RootProvider::new_http(anvil.endpoint_url());

        let url_provider = anvil.endpoint_url();

        let robust = RobustProviderBuilder::new(fill_provider)
            .fallback(root_provider)
            .fallback(url_provider.clone())
            .fallback(url_provider)
            .build()
            .await?;

        assert_eq!(robust.fallback_providers.len(), 3);

        Ok(())
    }
}
