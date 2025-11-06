use std::{future::Future, sync::Arc, time::Duration};

use alloy::{
    eips::{BlockId, BlockNumberOrTag},
    network::Network,
    providers::{Provider, RootProvider},
    pubsub::Subscription,
    rpc::types::{Filter, Log},
    transports::{RpcError, TransportErrorKind},
};
use backon::{ExponentialBuilder, Retryable};
use thiserror::Error;
use tokio::time::{error as TokioError, timeout};
use tracing::{error, info};

#[derive(Error, Debug, Clone)]
pub enum Error {
    #[error("Operation timed out")]
    Timeout,
    #[error("RPC call failed after exhausting all retry attempts: {0}")]
    RpcError(Arc<RpcError<TransportErrorKind>>),
    #[error("Block not found, Block Id: {0}")]
    BlockNotFound(BlockId),
}

impl From<RpcError<TransportErrorKind>> for Error {
    fn from(err: RpcError<TransportErrorKind>) -> Self {
        Error::RpcError(Arc::new(err))
    }
}

impl From<TokioError::Elapsed> for Error {
    fn from(_: TokioError::Elapsed) -> Self {
        Error::Timeout
    }
}

/// Provider wrapper with built-in retry and timeout mechanisms.
///
/// This wrapper around Alloy providers automatically handles retries,
/// timeouts, and error logging for RPC calls.
/// The first provider in the vector is treated as the primary provider.
#[derive(Clone)]
pub struct RobustProvider<N: Network> {
    providers: Vec<RootProvider<N>>,
    max_timeout: Duration,
    max_retries: usize,
    min_delay: Duration,
}

// RPC retry and timeout settings
/// Default timeout used by `RobustProvider`
pub const DEFAULT_MAX_TIMEOUT: Duration = Duration::from_secs(60);
/// Default maximum number of retry attempts.
pub const DEFAULT_MAX_RETRIES: usize = 3;
/// Default base delay between retries.
pub const DEFAULT_MIN_DELAY: Duration = Duration::from_secs(1);

impl<N: Network> RobustProvider<N> {
    /// Create a new `RobustProvider` with default settings.
    /// The provided provider is treated as the primary provider.
    #[must_use]
    pub fn new(provider: impl Provider<N>) -> Self {
        Self {
            providers: vec![provider.root().to_owned()],
            max_timeout: DEFAULT_MAX_TIMEOUT,
            max_retries: DEFAULT_MAX_RETRIES,
            min_delay: DEFAULT_MIN_DELAY,
        }
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
    pub fn min_delay(mut self, retry_interval: Duration) -> Self {
        self.min_delay = retry_interval;
        self
    }

    /// Get a reference to the primary provider (the first provider in the list)
    ///
    /// # Panics
    ///
    /// If there are no providers set (this should never happen)
    #[must_use]
    pub fn root(&self) -> &RootProvider<N> {
        // Safe to unwrap because we always have at least one provider
        self.providers.first().expect("providers vector should never be empty")
    }

    /// Add a fallback provider to the list.
    ///
    /// Fallback providers are used when the primary provider times out or fails.
    #[must_use]
    pub fn fallback(mut self, provider: RootProvider<N>) -> Self {
        self.providers.push(provider);
        self
    }

    /// Fetch a block by number with retry and timeout.
    ///
    /// # Errors
    ///
    /// Returns an error if RPC call fails repeatedly even
    /// after exhausting retries or if the call times out.
    pub async fn get_block_by_number(
        &self,
        number: BlockNumberOrTag,
    ) -> Result<N::BlockResponse, Error> {
        info!("eth_getBlockByNumber called");
        let result = self
            .retry_with_total_timeout(move |provider| async move {
                provider.get_block_by_number(number).await
            })
            .await;
        if let Err(e) = &result {
            error!(error = %e, "eth_getByBlockNumber failed");
        }

        result?.ok_or_else(|| Error::BlockNotFound(number.into()))
    }

    /// Fetch the latest block number with retry and timeout.
    ///
    /// # Errors
    ///
    /// Returns an error if RPC call fails repeatedly even
    /// after exhausting retries or if the call times out.
    pub async fn get_block_number(&self) -> Result<u64, Error> {
        info!("eth_getBlockNumber called");
        let result = self
            .retry_with_total_timeout(
                move |provider| async move { provider.get_block_number().await },
            )
            .await;
        if let Err(e) = &result {
            error!(error = %e, "eth_getBlockNumber failed");
        }
        result
    }

    /// Fetch a block by hash with retry and timeout.
    ///
    /// # Errors
    ///
    /// Returns an error if RPC call fails repeatedly even
    /// after exhausting retries or if the call times out.
    pub async fn get_block_by_hash(
        &self,
        hash: alloy::primitives::BlockHash,
    ) -> Result<N::BlockResponse, Error> {
        info!("eth_getBlockByHash called");
        let result = self
            .retry_with_total_timeout(move |provider| async move {
                provider.get_block_by_hash(hash).await
            })
            .await;
        if let Err(e) = &result {
            error!(error = %e, "eth_getBlockByHash failed");
        }

        result?.ok_or_else(|| Error::BlockNotFound(hash.into()))
    }

    /// Fetch logs for the given filter with retry and timeout.
    ///
    /// # Errors
    ///
    /// Returns an error if RPC call fails repeatedly even
    /// after exhausting retries or if the call times out.
    pub async fn get_logs(&self, filter: &Filter) -> Result<Vec<Log>, Error> {
        info!("eth_getLogs called");
        let result = self
            .retry_with_total_timeout(
                move |provider| async move { provider.get_logs(filter).await },
            )
            .await;
        if let Err(e) = &result {
            error!(error = %e, "eth_getLogs failed");
        }
        result
    }

    /// Subscribe to new block headers with retry and timeout.
    ///
    /// # Errors
    ///
    /// Returns an error if RPC call fails repeatedly even
    /// after exhausting retries or if the call times out.
    pub async fn subscribe_blocks(&self) -> Result<Subscription<N::HeaderResponse>, Error> {
        info!("eth_subscribe called");
        // We need this otherwise error is not clear
        self.root().client().expect_pubsub_frontend();
        let result = self
            .retry_with_total_timeout(
                move |provider| async move { provider.subscribe_blocks().await },
            )
            .await;
        if let Err(e) = &result {
            error!(error = %e, "eth_subscribe failed");
        }
        result
    }

    /// Execute `operation` with exponential backoff and a total timeout.
    ///
    /// Wraps the retry logic with `tokio::time::timeout(self.max_timeout, ...)` so
    /// the entire operation (including time spent inside the RPC call) cannot exceed
    /// `max_timeout`.
    ///
    /// If the timeout is exceeded and fallback providers are available, it will
    /// attempt to use each fallback provider in sequence.
    ///
    /// # Errors
    ///
    /// - Returns [`RpcError<TransportErrorKind>`] with message "total operation timeout exceeded
    ///   and all fallback providers failed" if the overall timeout elapses and no fallback
    ///   providers succeed.
    /// - Propagates any [`RpcError<TransportErrorKind>`] from the underlying retries.
    async fn retry_with_total_timeout<T, F, Fut>(&self, operation: F) -> Result<T, Error>
    where
        F: Fn(RootProvider<N>) -> Fut,
        Fut: Future<Output = Result<T, RpcError<TransportErrorKind>>>,
    {
        let mut last_error = None;

        // Try each provider in sequence (first one is primary)
        for (idx, provider) in self.providers.iter().enumerate() {
            if idx == 0 {
                info!("Attempting primary provider");
            } else {
                info!("Attempting fallback provider {} out of {}", idx, self.providers.len() - 1);
            }

            let result = self.try_provider_with_timeout(provider, &operation).await;

            match result {
                Ok(value) => {
                    if idx > 0 {
                        info!(provider_num = idx, "Fallback provider succeeded");
                    }
                    return Ok(value);
                }
                Err(e) => {
                    last_error = Some(e);
                    if idx == 0 && self.providers.len() > 1 {
                        info!("Primary provider failed, trying fallback provider(s)");
                    } else {
                        error!(provider_num = idx, err = %last_error.as_ref().unwrap(), "Fallback provider failed with error");
                    }
                }
            }
        }

        error!("All providers failed or timed out");
        // Return the last error encountered
        Err(last_error.unwrap_or(Error::Timeout))
    }

    /// Try executing an operation with a specific provider with retry and timeout.
    async fn try_provider_with_timeout<T, F, Fut>(
        &self,
        provider: &RootProvider<N>,
        operation: F,
    ) -> Result<T, Error>
    where
        F: Fn(RootProvider<N>) -> Fut,
        Fut: Future<Output = Result<T, RpcError<TransportErrorKind>>>,
    {
        let retry_strategy = ExponentialBuilder::default()
            .with_max_times(self.max_retries)
            .with_min_delay(self.min_delay);

        timeout(
            self.max_timeout,
            (|| operation(provider.clone()))
                .retry(retry_strategy)
                .notify(|err: &RpcError<TransportErrorKind>, dur: Duration| {
                    info!(error = %err, "RPC error retrying after {:?}", dur);
                })
                .sleep(tokio::time::sleep),
        )
        .await
        .map_err(Error::from)?
        .map_err(Error::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::network::Ethereum;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::time::sleep;

    fn test_provider(
        timeout: u64,
        max_retries: usize,
        retry_interval: u64,
    ) -> RobustProvider<Ethereum> {
        RobustProvider {
            providers: vec![RootProvider::new_http("http://localhost:8545".parse().unwrap())],
            max_timeout: Duration::from_millis(timeout),
            max_retries,
            min_delay: Duration::from_millis(retry_interval),
        }
    }

    #[tokio::test]
    async fn test_retry_with_timeout_succeeds_on_first_attempt() {
        let provider = test_provider(100, 3, 10);

        let call_count = AtomicUsize::new(0);

        let result = provider
            .retry_with_total_timeout(|_| async {
                call_count.fetch_add(1, Ordering::SeqCst);
                let count = call_count.load(Ordering::SeqCst);
                Ok(count)
            })
            .await;

        assert!(matches!(result, Ok(1)));
    }

    #[tokio::test]
    async fn test_retry_with_timeout_retries_on_error() {
        let provider = test_provider(100, 3, 10);

        let call_count = AtomicUsize::new(0);

        let result = provider
            .retry_with_total_timeout(|_| async {
                call_count.fetch_add(1, Ordering::SeqCst);
                let count = call_count.load(Ordering::SeqCst);
                match count {
                    3 => Ok(count),
                    _ => Err(TransportErrorKind::BackendGone.into()),
                }
            })
            .await;

        assert!(matches!(result, Ok(3)));
    }

    #[tokio::test]
    async fn test_retry_with_timeout_fails_after_max_retries() {
        let provider = test_provider(100, 2, 10);

        let call_count = AtomicUsize::new(0);

        let result: Result<(), Error> = provider
            .retry_with_total_timeout(|_| async {
                call_count.fetch_add(1, Ordering::SeqCst);
                Err(TransportErrorKind::BackendGone.into())
            })
            .await;

        assert!(matches!(result, Err(Error::RpcError(_))));
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_retry_with_timeout_respects_max_timeout() {
        let max_timeout = 50;
        let provider = test_provider(max_timeout, 10, 1);

        let result = provider
            .retry_with_total_timeout(move |_provider| async move {
                sleep(Duration::from_millis(max_timeout + 10)).await;
                Ok(42)
            })
            .await;

        assert!(matches!(result, Err(Error::Timeout)));
    }
}
