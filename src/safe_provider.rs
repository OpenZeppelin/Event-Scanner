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

use std::{sync::Arc, time::Duration};

use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    providers::{Provider, RootProvider},
    transports::{RpcError, TransportErrorKind},
};
use backon::{ExponentialBuilder, Retryable};
use thiserror::Error;
use tokio::time::timeout;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_RETRIES: usize = 5;
const DEFAULT_RETRY_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Error, Debug, Clone)]
pub enum SafeProviderError {
    #[error("RPC error: {0}")]
    RpcError(Arc<RpcError<TransportErrorKind>>),

    #[error("Request timeout after {0:?}")]
    Timeout(Duration),

    #[error("Block not found: {0}")]
    BlockNotFound(BlockNumberOrTag),

    #[error("All retry attempts exhausted")]
    RetryExhausted,
}

impl From<RpcError<TransportErrorKind>> for SafeProviderError {
    fn from(error: RpcError<TransportErrorKind>) -> Self {
        SafeProviderError::RpcError(Arc::new(error))
    }
}

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
    ) -> Result<Option<N::BlockResponse>, SafeProviderError> {
        let provider = self.provider.clone();
        self.retry_with_timeout(|| async { provider.get_block_by_number(number).await }).await
    }

    #[allow(clippy::missing_errors_doc)]
    pub async fn get_block_number(&self) -> Result<u64, SafeProviderError> {
        let provider = self.provider.clone();
        self.retry_with_timeout(|| async { provider.get_block_number().await }).await
    }

    #[allow(clippy::missing_errors_doc)]
    pub async fn get_block_by_hash(
        &self,
        hash: alloy::primitives::BlockHash,
    ) -> Result<Option<N::BlockResponse>, SafeProviderError> {
        let provider = self.provider.clone();
        self.retry_with_timeout(|| async { provider.get_block_by_hash(hash).await }).await
    }

    #[allow(clippy::missing_errors_doc)]
    async fn retry_with_timeout<T, F, Fut>(&self, operation: F) -> Result<T, SafeProviderError>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<T, RpcError<TransportErrorKind>>>,
    {
        let timeout_duration = self.timeout;

        let wrapped_operation = || async {
            match timeout(timeout_duration, operation()).await {
                Ok(Ok(result)) => Ok(result),
                Ok(Err(e)) => Err(SafeProviderError::from(e)),
                Err(_) => Err(SafeProviderError::Timeout(timeout_duration)),
            }
        };

        let retry_strategy = ExponentialBuilder::default()
            .with_max_times(self.max_retries)
            .with_min_delay(self.retry_interval);

        wrapped_operation.retry(retry_strategy).sleep(tokio::time::sleep).await
    }
}

