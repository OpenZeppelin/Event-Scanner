use std::{marker::PhantomData, time::Duration};

use alloy::{network::Network, providers::Provider};

use crate::{
    RobustProvider,
    robust_provider::{error::Error, types::IntoProvider},
};

// RPC retry and timeout settings
/// Default timeout used by `RobustProvider`
pub const DEFAULT_MAX_TIMEOUT: Duration = Duration::from_secs(60);
/// Default maximum number of retry attempts.
pub const DEFAULT_MAX_RETRIES: usize = 3;
/// Default base delay between retries.
pub const DEFAULT_MIN_DELAY: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct RobustProviderBuilder<N: Network, P: IntoProvider<N>> {
    providers: Vec<P>,
    max_timeout: Duration,
    max_retries: usize,
    min_delay: Duration,
    _network: PhantomData<N>,
}

impl<N: Network, P: IntoProvider<N>> RobustProviderBuilder<N, P> {
    /// Create a new `RobustProvider` with default settings.
    ///
    /// The provided provider is treated as the primary provider.
    #[must_use]
    pub fn new(provider: P) -> Self {
        Self {
            providers: vec![provider],
            max_timeout: DEFAULT_MAX_TIMEOUT,
            max_retries: DEFAULT_MAX_RETRIES,
            min_delay: DEFAULT_MIN_DELAY,
            _network: PhantomData,
        }
    }

    /// Create a new `RobustProvider` with no retry attempts and only timeout set.
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
    pub fn max_timeout(mut self, timeout: Duration) -> Self {
        self.max_timeout = timeout;
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
            max_timeout: self.max_timeout,
            max_retries: self.max_retries,
            min_delay: self.min_delay,
        })
    }
}
