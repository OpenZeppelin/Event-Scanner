use std::{future::Future, time::Duration};

use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    providers::{Provider, RootProvider},
    pubsub::Subscription,
    rpc::types::{Filter, Log},
    transports::{RpcError, TransportErrorKind},
};
use backon::{ExponentialBuilder, Retryable};
use tracing::{error, info};

/// Safe provider wrapper with built-in retry and timeout mechanisms.
///
/// This wrapper around Alloy providers automatically handles retries,
/// timeouts, and error logging for RPC calls.
#[derive(Clone)]
pub struct SafeProvider<N: Network> {
    provider: RootProvider<N>,
    max_timeout: Duration,
    max_retries: usize,
    retry_interval: Duration,
    fallback_providers: Vec<RootProvider<N>>,
}

// RPC retry and timeout settings
/// Default timeout used by `SafeProvider`
pub const DEFAULT_MAX_TIMEOUT: Duration = Duration::from_secs(30);
/// Default maximum number of retry attempts.
pub const DEFAULT_MAX_RETRIES: usize = 5;
/// Default base delay between retries.
pub const DEFAULT_RETRY_INTERVAL: Duration = Duration::from_secs(1);

impl<N: Network> SafeProvider<N> {
    /// Create a new `SafeProvider` with default settings.
    #[must_use]
    pub fn new(provider: RootProvider<N>) -> Self {
        Self {
            provider,
            max_timeout: DEFAULT_MAX_TIMEOUT,
            max_retries: DEFAULT_MAX_RETRIES,
            retry_interval: DEFAULT_RETRY_INTERVAL,
            fallback_providers: Vec::new(),
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

    /// Add a fallback provider to the list.
    ///
    /// Fallback providers are used when the primary provider times out.
    #[must_use]
    pub fn fallback_provider(mut self, provider: RootProvider<N>) -> Self {
        self.fallback_providers.push(provider);
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
    ) -> Result<Option<N::BlockResponse>, RpcError<TransportErrorKind>> {
        info!("eth_getBlockByNumber called");
        let result = self
            .retry_with_total_timeout(move |provider| async move {
                provider.get_block_by_number(number).await
            })
            .await;
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
    pub async fn get_block_number(&self) -> Result<u64, RpcError<TransportErrorKind>> {
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
    ) -> Result<Option<N::BlockResponse>, RpcError<TransportErrorKind>> {
        info!("eth_getBlockByHash called");
        let result = self
            .retry_with_total_timeout(move |provider| async move {
                provider.get_block_by_hash(hash).await
            })
            .await;
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
    pub async fn get_logs(
        &self,
        filter: &Filter,
    ) -> Result<Vec<Log>, RpcError<TransportErrorKind>> {
        info!("eth_getLogs called");
        let result = self
            .retry_with_total_timeout(move |provider| {
                let filter = filter.clone();
                async move { provider.get_logs(&filter).await }
            })
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
    pub async fn subscribe_blocks(
        &self,
    ) -> Result<Subscription<N::HeaderResponse>, RpcError<TransportErrorKind>> {
        info!("eth_subscribe called");
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
    async fn retry_with_total_timeout<T, F, Fut>(
        &self,
        operation: F,
    ) -> Result<T, RpcError<TransportErrorKind>>
    where
        F: Fn(RootProvider<N>) -> Fut,
        Fut: Future<Output = Result<T, RpcError<TransportErrorKind>>>,
    {
        // Try primary provider first
        let result = self.try_provider_with_timeout(&self.provider, &operation).await;

        if let Ok(value) = result {
            return Ok(value);
        }

        if result.is_err() && self.fallback_providers.is_empty() {
            return result;
        }

        info!(
            "Primary provider failed, trying {} fallback provider(s)",
            self.fallback_providers.len()
        );

        // Try each fallback provider
        for (idx, fallback_provider) in self.fallback_providers.iter().enumerate() {
            info!("Attempting fallback provider {}", idx + 1);

            let fallback_result =
                self.try_provider_with_timeout(fallback_provider, &operation).await;

            match fallback_result {
                Ok(value) => {
                    info!("Fallback provider {} succeeded", idx + 1);
                    return Ok(value);
                }
                Err(e) => {
                    error!("Fallback provider {} failed with error: {}", idx + 1, e);
                }
            }
        }

        error!("All fallback providers failed or timed out");
        Err(TransportErrorKind::custom_str(
            "total operation timeout exceeded and all fallback providers failed",
        ))
    }

    /// Try executing an operation with a specific provider with retry and timeout.
    async fn try_provider_with_timeout<T, F, Fut>(
        &self,
        provider: &RootProvider<N>,
        operation: F,
    ) -> Result<T, RpcError<TransportErrorKind>>
    where
        F: Fn(RootProvider<N>) -> Fut,
        Fut: Future<Output = Result<T, RpcError<TransportErrorKind>>>,
    {
        let retry_strategy = ExponentialBuilder::default()
            .with_max_times(self.max_retries)
            .with_min_delay(self.retry_interval);

        match tokio::time::timeout(
            self.max_timeout,
            (|| operation(provider.clone())).retry(retry_strategy).sleep(tokio::time::sleep),
        )
        .await
        {
            Ok(res) => res,
            Err(_) => Err(TransportErrorKind::custom_str("total operation timeout exceeded")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::network::Ethereum;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::time::sleep;

    fn test_provider(
        timeout: u64,
        max_retries: usize,
        retry_interval: u64,
    ) -> SafeProvider<Ethereum> {
        SafeProvider {
            provider: RootProvider::new_http("http://localhost:8545".parse().unwrap()),
            max_timeout: Duration::from_millis(timeout),
            max_retries,
            retry_interval: Duration::from_millis(retry_interval),
            fallback_providers: Vec::new(),
        }
    }

    #[tokio::test]
    async fn test_retry_with_timeout_succeeds_on_first_attempt() {
        let provider = test_provider(100, 3, 10);

        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        let result = provider
            .retry_with_total_timeout(move |_provider| {
                let call_count = call_count_clone.clone();
                async move {
                    call_count.fetch_add(1, Ordering::SeqCst);
                    Ok(42)
                }
            })
            .await;

        assert!(matches!(result, Ok(42)));
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_retry_with_timeout_retries_on_error() {
        let provider = test_provider(100, 3, 10);

        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        let result = provider
            .retry_with_total_timeout(move |_provider| {
                let call_count = call_count_clone.clone();
                async move {
                    call_count.fetch_add(1, Ordering::SeqCst);
                    if call_count.load(Ordering::SeqCst) < 3 {
                        Err(TransportErrorKind::custom_str("temporary error"))
                    } else {
                        Ok(42)
                    }
                }
            })
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_retry_with_timeout_fails_after_max_retries() {
        let provider = test_provider(100, 2, 10);

        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        let result = provider
            .retry_with_total_timeout(move |_provider| {
                let call_count = call_count_clone.clone();
                async move {
                    call_count.fetch_add(1, Ordering::SeqCst);
                    Err::<i32, RpcError<TransportErrorKind>>(TransportErrorKind::custom_str(
                        "permanent error",
                    ))
                }
            })
            .await;

        let err = result.unwrap_err();
        assert!(err.to_string().contains("permanent error"),);
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_retry_with_timeout_respects_total_delay() {
        let max_timeout = 50;
        let provider = test_provider(max_timeout, 10, 1);

        let result = provider
            .retry_with_total_timeout(move |_provider| async move {
                sleep(Duration::from_millis(max_timeout + 10)).await;
                Ok(42)
            })
            .await;

        let err = result.unwrap_err();
        assert!(err.to_string().contains("total operation timeout exceeded"),);
    }
}
