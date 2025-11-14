use std::{marker::PhantomData, time::Duration};

use alloy::{network::Network, providers::Provider};

use crate::robust_provider::{
    Error, IntoProvider, RobustProvider, subscription::DEFAULT_RECONNECT_INTERVAL,
};

// RPC retry and timeout settings
/// Default timeout used by `RobustProvider`
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Default timeout for subscriptions
pub const DEFAULT_SUBSCRIPTION_TIMEOUT: Duration = Duration::from_secs(120);
/// Default maximum number of retry attempts.
pub const DEFAULT_MAX_RETRIES: usize = 3;
/// Default base delay between retries.
pub const DEFAULT_MIN_DELAY: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct RobustProviderBuilder<N: Network, P: IntoProvider<N>> {
    providers: Vec<P>,
    call_timeout: Duration,
    subscription_timeout: Duration,
    max_retries: usize,
    min_delay: Duration,
    reconnect_interval: Duration,
    _network: PhantomData<N>,
}

impl<N: Network, P: IntoProvider<N>> RobustProviderBuilder<N, P> {
    /// Create a new [`RobustProvider`] with default settings.
    ///
    /// The provided provider is treated as the primary provider.
    #[must_use]
    pub fn new(provider: P) -> Self {
        Self {
            providers: vec![provider],
            call_timeout: DEFAULT_CALL_TIMEOUT,
            subscription_timeout: DEFAULT_SUBSCRIPTION_TIMEOUT,
            max_retries: DEFAULT_MAX_RETRIES,
            min_delay: DEFAULT_MIN_DELAY,
            reconnect_interval: DEFAULT_RECONNECT_INTERVAL,
            _network: PhantomData,
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
    pub fn fallback(mut self, provider: P) -> Self {
        self.providers.push(provider);
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
        let mut providers = vec![];
        for p in self.providers {
            providers.push(p.into_provider().await?.root().to_owned());
        }
        Ok(RobustProvider {
            providers,
            call_timeout: self.call_timeout,
            subscription_timeout: self.subscription_timeout,
            max_retries: self.max_retries,
            min_delay: self.min_delay,
            reconnect_interval: self.reconnect_interval,
        })
    }
}
