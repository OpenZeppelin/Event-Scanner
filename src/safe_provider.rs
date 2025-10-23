//! Safe provider wrapper with built-in retry and timeout mechanisms.
//!
//! This module provides a wrapper around Alloy providers that automatically
//! handles retries, timeouts, and error logging for RPC calls.
//!
//! # Example
//!
//! ```rust,no_run
//! use alloy::{
//!     network::Ethereum,
//!     providers::{RootProvider, WsConnect},
//!     rpc::client::ClientBuilder,
//! };
//! use event_scanner::safe_provider::SafeProvider;
//! use std::time::Duration;
//!
//! async fn example() -> Result<(), Box<dyn std::error::Error>> {
//!     let provider = RootProvider::<Ethereum>::new(
//!         ClientBuilder::default().ws(WsConnect::new("wss://localhost:8000")).await?,
//!     );
//!     let safe_provider =
//!         SafeProvider::new(provider).max_timeout(Duration::from_secs(30)).max_retries(5);
//!
//!     let block = safe_provider.get_block_by_number(12345.into()).await?;
//!     Ok(())
//! }
//! ```

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

// RPC retry and timeout settings
/// Default timeout used by `SafeProvider`
pub const DEFAULT_MAX_TIMEOUT: Duration = Duration::from_secs(30);
/// Default maximum number of retry attempts.
pub const DEFAULT_MAX_RETRIES: usize = 5;
/// Default base delay between retries.
pub const DEFAULT_RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// Provider wrapper adding retries and timeouts.
#[derive(Clone)]
pub struct SafeProvider<N: Network> {
    provider: RootProvider<N>,
    max_timeout: Duration,
    max_retries: usize,
    retry_interval: Duration,
}

impl<N: Network> SafeProvider<N> {
    /// Create a new `SafeProvider` with default settings.
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
    ) -> Result<Option<N::BlockResponse>, RpcError<TransportErrorKind>> {
        info!("eth_getBlockByNumber called");
        let provider = self.provider.clone();
        let result = self
            .retry_with_total_timeout(|| async { provider.get_block_by_number(number).await })
            .await;
        if let Err(e) = &result {
            error!("eth_getByBlockNumber failed: {}", e);
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
        let operation = || self.provider.get_block_number();
        let result = self.retry_with_total_timeout(operation).await;
        if let Err(e) = &result {
            error!("eth_getBlockNumber failed: {}", e);
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
        let provider = self.provider.clone();
        let result = self
            .retry_with_total_timeout(|| async { provider.get_block_by_hash(hash).await })
            .await;
        if let Err(e) = &result {
            error!("eth_getBlockByHash failed: {}", e);
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
        let provider = self.provider.clone();
        let result =
            self.retry_with_total_timeout(|| async { provider.get_logs(filter).await }).await;
        if let Err(e) = &result {
            error!("eth_getLogs failed: {}", e);
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
        let provider = self.provider.clone();
        let result =
            self.retry_with_total_timeout(|| async { provider.subscribe_blocks().await }).await;
        if let Err(e) = &result {
            error!("eth_subscribe failed: {}", e);
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
    ) -> Result<T, RpcError<TransportErrorKind>>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<T, RpcError<TransportErrorKind>>>,
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
            Ok(res) => res,
            Err(_) => Err(TransportErrorKind::custom_str("total operation timeout exceeded")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::network::Ethereum;
    use std::sync::{Arc, Mutex};
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
        }
    }

    #[tokio::test]
    async fn test_retry_with_timeout_succeeds_on_first_attempt() {
        let provider = test_provider(100, 3, 10);

        let call_count = AtomicUsize::new(0);

        let result = provider
            .retry_with_timeout(|| async {
                call_count.fetch_add(1, Ordering::SeqCst);
                Ok(42)
            })
            .await;

        assert!(matches!(result, Ok(42)));
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_retry_with_timeout_retries_on_error() {
        let provider =
            create_test_provider(Duration::from_millis(100), 3, Duration::from_millis(10));

        let call_count = Arc::new(Mutex::new(0));
        let call_count_clone = call_count.clone();

        let result = provider
            .retry_with_total_timeout(move || {
                let count = call_count_clone.clone();
                async move {
                    let mut c = count.lock().unwrap();
                    *c += 1;
                    if *c < 3 {
                        Err(TransportErrorKind::custom_str("temporary error"))
                    } else {
                        Ok(42)
                    }
                }
            })
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
        assert_eq!(*call_count.lock().unwrap(), 3);
    }

    #[tokio::test]
    async fn test_retry_with_timeout_fails_after_max_retries() {
        let provider =
            create_test_provider(Duration::from_millis(100), 2, Duration::from_millis(10));

        let call_count = Arc::new(Mutex::new(0));
        let call_count_clone = call_count.clone();

        let result = provider
            .retry_with_total_timeout(move || {
                let count = call_count_clone.clone();
                async move {
                    let mut c = count.lock().unwrap();
                    *c += 1;
                    Err::<i32, RpcError<TransportErrorKind>>(TransportErrorKind::custom_str(
                        "permanent error",
                    ))
                }
            })
            .await;

        assert!(result.is_err());
        assert_eq!(*call_count.lock().unwrap(), 3);
    }

    #[tokio::test]
    async fn test_retry_with_timeout_respects_total_delay() {
        let max_timeout = Duration::from_millis(50);
        let provider = create_test_provider(max_timeout, 10, Duration::from_millis(1));

        let result = provider
            .retry_with_total_timeout(move || async move {
                sleep(max_timeout + Duration::from_millis(10)).await;
                Ok(42)
            })
            .await;

        assert!(result.is_err());
    }
}
