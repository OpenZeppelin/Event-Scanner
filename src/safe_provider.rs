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
//!         SafeProvider::new(provider).with_timeout(Duration::from_secs(30)).with_max_retries(5);
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
use tracing::{debug, error};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_MAX_RETRIES: usize = 5;
pub const DEFAULT_RETRY_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct SafeProvider<N: Network> {
    provider: RootProvider<N>,
    timeout: Duration,
    max_retries: usize,
    retry_interval: Duration,
}

impl<N: Network> SafeProvider<N> {
    #[must_use]
    pub fn new(provider: RootProvider<N>) -> Self {
        Self {
            provider,
            timeout: DEFAULT_TIMEOUT,
            max_retries: DEFAULT_MAX_RETRIES,
            retry_interval: DEFAULT_RETRY_INTERVAL,
        }
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    #[must_use]
    pub fn with_max_retries(mut self, max_retries: usize) -> Self {
        self.max_retries = max_retries;
        self
    }

    #[must_use]
    pub fn with_retry_interval(mut self, retry_interval: Duration) -> Self {
        self.retry_interval = retry_interval;
        self
    }

    #[must_use]
    pub fn inner(&self) -> &RootProvider<N> {
        &self.provider
    }

    #[allow(clippy::missing_errors_doc)]
    pub async fn get_block_by_number(
        &self,
        number: BlockNumberOrTag,
    ) -> Result<Option<N::BlockResponse>, RpcError<TransportErrorKind>> {
        debug!("SafeProvider eth_getBlockByNumber called with number: {:?}", number);
        let provider = self.provider.clone();
        let result =
            self.retry_with_timeout(|| async { provider.get_block_by_number(number).await }).await;
        if let Err(e) = &result {
            error!("SafeProvider eth_getByBlockNumber failed: {}", e);
        }
        result
    }

    #[allow(clippy::missing_errors_doc)]
    pub async fn get_block_number(&self) -> Result<u64, RpcError<TransportErrorKind>> {
        debug!("SafeProvider eth_getBlockNumber called");
        let provider = self.provider.clone();
        let result = self.retry_with_timeout(|| async { provider.get_block_number().await }).await;
        if let Err(e) = &result {
            error!("SafeProvider eth_getBlockNumber failed: {}", e);
        }
        result
    }

    #[allow(clippy::missing_errors_doc)]
    pub async fn get_block_by_hash(
        &self,
        hash: alloy::primitives::BlockHash,
    ) -> Result<Option<N::BlockResponse>, RpcError<TransportErrorKind>> {
        debug!("SafeProvider eth_getBlockByHash called with hash: {:?}", hash);
        let provider = self.provider.clone();
        let result =
            self.retry_with_timeout(|| async { provider.get_block_by_hash(hash).await }).await;
        if let Err(e) = &result {
            error!("SafeProvider eth_getBlockByHash failed: {}", e);
        }
        result
    }

    #[allow(clippy::missing_errors_doc)]
    pub async fn get_logs(
        &self,
        filter: &Filter,
    ) -> Result<Vec<Log>, RpcError<TransportErrorKind>> {
        debug!("eth_getLogs called with filter: {:?}", filter);
        let provider = self.provider.clone();
        let result = self.retry_with_timeout(|| async { provider.get_logs(filter).await }).await;
        if let Err(e) = &result {
            error!("SafeProvider eth_getLogs failed: {}", e);
        }
        result
    }

    #[allow(clippy::missing_errors_doc)]
    pub async fn subscribe_blocks(
        &self,
    ) -> Result<Subscription<N::HeaderResponse>, RpcError<TransportErrorKind>> {
        debug!("eth_subscribe called");
        let provider = self.provider.clone();
        let result = self.retry_with_timeout(|| async { provider.subscribe_blocks().await }).await;
        if let Err(e) = &result {
            error!("SafeProvider eth_subscribe failed: {}", e);
        }
        result
    }

    #[allow(clippy::missing_errors_doc)]
    pub(crate) async fn retry_with_timeout<T, F, Fut>(
        &self,
        operation: F,
    ) -> Result<T, RpcError<TransportErrorKind>>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<T, RpcError<TransportErrorKind>>>,
    {
        let retry_strategy = ExponentialBuilder::default()
            .with_max_times(self.max_retries)
            .with_total_delay(Some(self.timeout))
            .with_min_delay(self.retry_interval);

        operation.retry(retry_strategy).sleep(tokio::time::sleep).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::network::Ethereum;
    use std::sync::{Arc, Mutex};

    fn create_test_provider(
        timeout: Duration,
        max_retries: usize,
        retry_interval: Duration,
    ) -> SafeProvider<Ethereum> {
        SafeProvider {
            provider: RootProvider::<Ethereum>::new_http("http://localhost:8545".parse().unwrap()),
            timeout,
            max_retries,
            retry_interval,
        }
    }

    #[tokio::test]
    async fn test_retry_with_timeout_succeeds_on_first_attempt() {
        let provider =
            create_test_provider(Duration::from_millis(100), 3, Duration::from_millis(10));

        let call_count = Arc::new(Mutex::new(0));
        let call_count_clone = call_count.clone();

        let result = provider
            .retry_with_timeout(move || {
                let count = call_count_clone.clone();
                async move {
                    let mut c = count.lock().unwrap();
                    *c += 1;
                    Ok(42)
                }
            })
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
        assert_eq!(*call_count.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn test_retry_with_timeout_retries_on_error() {
        let provider =
            create_test_provider(Duration::from_millis(100), 3, Duration::from_millis(10));

        let call_count = Arc::new(Mutex::new(0));
        let call_count_clone = call_count.clone();

        let result = provider
            .retry_with_timeout(move || {
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
            .retry_with_timeout(move || {
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
}
