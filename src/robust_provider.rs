use std::{future::Future, sync::Arc, time::Duration};

use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    providers::{Provider, RootProvider},
    pubsub::Subscription,
    rpc::types::{Filter, Log},
    transports::{RpcError, TransportErrorKind},
};
use backon::{ExponentialBuilder, Retryable};
use thiserror::Error;
use tracing::{error, info};

#[derive(Error, Debug, Clone)]
pub enum RobustProviderError {
    #[error("RPC error: {0}")]
    RpcError(Arc<RpcError<TransportErrorKind>>),
    #[error("Operation timed out")]
    Timeout,
    #[error("Retry failed after {0} tries")]
    RetryFail(usize),
}

impl From<RpcError<TransportErrorKind>> for RobustProviderError {
    fn from(err: RpcError<TransportErrorKind>) -> Self {
        RobustProviderError::RpcError(Arc::new(err))
    }
}

/// Safe provider wrapper with built-in retry and timeout mechanisms.
///
/// This wrapper around Alloy providers automatically handles retries,
/// timeouts, and error logging for RPC calls.
#[derive(Clone)]
pub struct RobustProvider<N: Network> {
    provider: RootProvider<N>,
    max_timeout: Duration,
    max_retries: usize,
    retry_interval: Duration,
}

// RPC retry and timeout settings
/// Default timeout used by `RobustProvider`
pub const DEFAULT_MAX_TIMEOUT: Duration = Duration::from_secs(30);
/// Default maximum number of retry attempts.
pub const DEFAULT_MAX_RETRIES: usize = 5;
/// Default base delay between retries.
pub const DEFAULT_RETRY_INTERVAL: Duration = Duration::from_secs(1);

impl<N: Network> RobustProvider<N> {
    /// Create a new `RobustProvider` with default settings.
    #[must_use]
    pub fn new(provider: RootProvider<N>) -> Self {
        Self {
            provider,
            max_timeout: DEFAULT_MAX_TIMEOUT,
            max_retries: DEFAULT_MAX_RETRIES,
            retry_interval: DEFAULT_RETRY_INTERVAL,
        }
    }

    #[must_use]
    pub fn max_timeout(mut self, timeout: Duration) -> Self {
        self.max_timeout = timeout;
        self
    }

    #[must_use]
    pub fn max_retries(mut self, max_retries: usize) -> Self {
        self.max_retries = max_retries;
        self
    }

    #[must_use]
    pub fn retry_interval(mut self, retry_interval: Duration) -> Self {
        self.retry_interval = retry_interval;
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
    ) -> Result<Option<N::BlockResponse>, RobustProviderError> {
        info!("eth_getBlockByNumber called");
        let operation = async || {
            self.provider.get_block_by_number(number).await.map_err(RobustProviderError::from)
        };
        let result = self.retry_with_total_timeout(operation).await;
        if let Err(e) = &result {
            error!(error = %e, "eth_getByBlockNumber failed");
        }
        result
    }

    /// Fetch the latest block number with retry and timeout.
    ///
    /// # Errors
    ///
    /// Returns an error if RPC call fails repeatedly even
    /// after exhausting retries or if the call times out.
    pub async fn get_block_number(&self) -> Result<u64, RobustProviderError> {
        info!("eth_getBlockNumber called");
        let operation =
            async || self.provider.get_block_number().await.map_err(RobustProviderError::from);
        let result = self.retry_with_total_timeout(operation).await;
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
    ) -> Result<Option<N::BlockResponse>, RobustProviderError> {
        info!("eth_getBlockByHash called");
        let operation =
            async || self.provider.get_block_by_hash(hash).await.map_err(RobustProviderError::from);
        let result = self.retry_with_total_timeout(operation).await;
        if let Err(e) = &result {
            error!(error = %e, "eth_getBlockByHash failed");
        }
        result
    }

    /// Fetch logs for the given filter with retry and timeout.
    ///
    /// # Errors
    ///
    /// Returns an error if RPC call fails repeatedly even
    /// after exhausting retries or if the call times out.
    pub async fn get_logs(&self, filter: &Filter) -> Result<Vec<Log>, RobustProviderError> {
        info!("eth_getLogs called");
        let operation =
            async || self.provider.get_logs(filter).await.map_err(RobustProviderError::from);
        let result = self.retry_with_total_timeout(operation).await;
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
    pub async fn subscribe_blocks(
        &self,
    ) -> Result<Subscription<N::HeaderResponse>, RobustProviderError> {
        info!("eth_subscribe called");
        let provider = self.provider.clone();
        let result = self
            .retry_with_total_timeout(|| async {
                provider.subscribe_blocks().await.map_err(RobustProviderError::from)
            })
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
    /// # Errors
    ///
    /// - Returns [`RpcError<TransportErrorKind>`] with message "total operation timeout exceeded"
    ///   if the overall timeout elapses.
    /// - Propagates any [`RpcError<TransportErrorKind>`] from the underlying retries.
    async fn retry_with_total_timeout<T, F, Fut>(
        &self,
        operation: F,
    ) -> Result<T, RobustProviderError>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<T, RobustProviderError>>,
    {
        let retry_strategy = ExponentialBuilder::default()
            .with_max_times(self.max_retries)
            .with_min_delay(self.retry_interval);

        match tokio::time::timeout(
            self.max_timeout,
            operation.retry(retry_strategy).sleep(tokio::time::sleep),
        )
        .await
        {
            Ok(Ok(res)) => Ok(res),
            Ok(Err(_)) => Err(RobustProviderError::RetryFail(self.max_retries + 1)),
            Err(_) => Err(RobustProviderError::Timeout),
        }
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
            provider: RootProvider::new_http("http://localhost:8545".parse().unwrap()),
            max_timeout: Duration::from_millis(timeout),
            max_retries,
            retry_interval: Duration::from_millis(retry_interval),
        }
    }

    #[tokio::test]
    async fn test_retry_with_timeout_succeeds_on_first_attempt() {
        let provider = test_provider(100, 3, 10);

        let call_count = AtomicUsize::new(0);

        let result = provider
            .retry_with_total_timeout(|| async {
                call_count.fetch_add(1, Ordering::SeqCst);
                Ok(call_count.load(Ordering::SeqCst))
            })
            .await;

        assert!(matches!(result, Ok(1)));
    }

    #[tokio::test]
    async fn test_retry_with_timeout_retries_on_error() {
        let provider = test_provider(100, 3, 10);

        let call_count = AtomicUsize::new(0);

        let result = provider
            .retry_with_total_timeout(|| async {
                call_count.fetch_add(1, Ordering::SeqCst);
                if call_count.load(Ordering::SeqCst) < 3 {
                    Err(RobustProviderError::RpcError(Arc::new(TransportErrorKind::custom_str(
                        "temp error",
                    ))))
                } else {
                    Ok(call_count.load(Ordering::SeqCst))
                }
            })
            .await;

        assert!(matches!(result, Ok(3)));
    }

    #[tokio::test]
    async fn test_retry_with_timeout_fails_after_max_retries() {
        let provider = test_provider(100, 2, 10);

        let call_count = AtomicUsize::new(0);

        let result = provider
            .retry_with_total_timeout(|| async {
                call_count.fetch_add(1, Ordering::SeqCst);
                // permanent error
                Err::<i32, RobustProviderError>(RobustProviderError::Timeout)
            })
            .await;

        let err = result.unwrap_err();
        assert!(matches!(err, RobustProviderError::RetryFail(3)));
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_retry_with_timeout_respects_total_delay() {
        let max_timeout = 50;
        let provider = test_provider(max_timeout, 10, 1);

        let result = provider
            .retry_with_total_timeout(|| async {
                sleep(Duration::from_millis(max_timeout + 10)).await;
                Ok(42)
            })
            .await;

        let err = result.unwrap_err();
        assert!(matches!(err, RobustProviderError::Timeout));
    }
}
