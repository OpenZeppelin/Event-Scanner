use std::{fmt::Debug, future::Future, sync::Arc, time::Duration};

use alloy::{
    eips::{BlockId, BlockNumberOrTag},
    network::{Ethereum, Network},
    providers::{
        DynProvider, Provider, RootProvider,
        fillers::{FillProvider, TxFiller},
        layers::{CacheProvider, CallBatchProvider},
    },
    pubsub::Subscription,
    rpc::types::{Filter, Log},
    transports::{RpcError, TransportErrorKind, http::reqwest::Url},
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
pub struct RobustProvider<N: Network = Ethereum> {
    providers: Vec<RootProvider<N>>,
    max_timeout: Duration,
    max_attempts: usize,
    min_delay: Duration,
}

pub trait IntoRobustProvider<N: Network = Ethereum> {
    fn into_robust_provider(
        self,
    ) -> impl std::future::Future<Output = Result<RobustProvider<N>, Error>> + Send;
}

impl<N: Network> IntoRobustProvider<N> for RootProvider<N> {
    fn into_robust_provider(
        self,
    ) -> impl std::future::Future<Output = Result<RobustProvider<N>, Error>> + Send {
        async move { Ok(RobustProvider::new(self)) }
    }
}

impl<N: Network> IntoRobustProvider<N> for RobustProvider<N> {
    fn into_robust_provider(
        self,
    ) -> impl std::future::Future<Output = Result<RobustProvider<N>, Error>> + Send {
        async move { Ok(self) }
    }
}

impl<N: Network> IntoRobustProvider<N> for &str {
    fn into_robust_provider(
        self,
    ) -> impl std::future::Future<Output = Result<RobustProvider<N>, Error>> + Send {
        async move { Ok(RobustProvider::new(RootProvider::connect(self).await?)) }
    }
}

impl<N: Network> IntoRobustProvider<N> for Url {
    fn into_robust_provider(
        self,
    ) -> impl std::future::Future<Output = Result<RobustProvider<N>, Error>> + Send {
        async move { Ok(RobustProvider::new(RootProvider::connect(self.as_str()).await?)) }
    }
}

impl<F, P, N> IntoRobustProvider<N> for FillProvider<F, P, N>
where
    F: TxFiller<N>,
    P: Provider<N>,
    N: Network,
{
    fn into_robust_provider(
        self,
    ) -> impl std::future::Future<Output = Result<RobustProvider<N>, Error>> + Send {
        async move { Ok(RobustProvider::new(self)) }
    }
}

impl<P, N> IntoRobustProvider<N> for CacheProvider<P, N>
where
    P: Provider<N>,
    N: Network,
{
    fn into_robust_provider(
        self,
    ) -> impl std::future::Future<Output = Result<RobustProvider<N>, Error>> + Send {
        async move { Ok(RobustProvider::new(self)) }
    }
}

impl<N> IntoRobustProvider<N> for DynProvider<N>
where
    N: Network,
{
    fn into_robust_provider(
        self,
    ) -> impl std::future::Future<Output = Result<RobustProvider<N>, Error>> + Send {
        async move { Ok(RobustProvider::new(self)) }
    }
}

impl<P, N> IntoRobustProvider<N> for CallBatchProvider<P, N>
where
    P: Provider<N> + 'static,
    N: Network,
{
    fn into_robust_provider(
        self,
    ) -> impl std::future::Future<Output = Result<RobustProvider<N>, Error>> + Send {
        async move { Ok(RobustProvider::new(self)) }
    }
}

// RPC retry and timeout settings
/// Default timeout used by `RobustProvider`
pub const DEFAULT_MAX_TIMEOUT: Duration = Duration::from_secs(60);
/// Default maximum number of retry attempts.
pub const DEFAULT_MAX_ATTEMPTS: usize = 3;
/// Default base delay between retries.
pub const DEFAULT_MIN_DELAY: Duration = Duration::from_secs(1);

impl<N: Network> RobustProvider<N> {
    /// Create a new `RobustProvider` with default settings.
    ///
    /// The provided provider is treated as the primary provider.
    #[must_use]
    pub fn new(provider: impl Provider<N>) -> Self {
        Self {
            providers: vec![provider.root().to_owned()],
            max_timeout: DEFAULT_MAX_TIMEOUT,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            min_delay: DEFAULT_MIN_DELAY,
        }
    }

    /// Create a new `RobustProvider` with no retry attempts and only timeout set.
    ///
    /// The provided provider is treated as the primary provider.
    #[must_use]
    pub fn no_retry(provider: impl Provider<N>) -> Self {
        Self {
            providers: vec![provider.root().to_owned()],
            max_timeout: DEFAULT_MAX_TIMEOUT,
            max_attempts: 1,
            min_delay: Duration::ZERO,
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
        self.max_attempts = max_retries + 1;
        self
    }

    /// Set the base delay for exponential backoff retries.
    #[must_use]
    pub fn min_delay(mut self, min_delay: Duration) -> Self {
        self.min_delay = min_delay;
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
    pub fn fallback(mut self, provider: impl Provider<N>) -> Self {
        self.providers.push(provider.root().to_owned());
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
            .retry_with_total_timeout(
                move |provider| async move { provider.get_block_by_number(number).await },
                false,
            )
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
                false,
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
            .retry_with_total_timeout(
                move |provider| async move { provider.get_block_by_hash(hash).await },
                false,
            )
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
                false,
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
        // immediately fail if primary does not support pubsub
        self.root().client().expect_pubsub_frontend();
        let result = self
            .retry_with_total_timeout(
                move |provider| async move { provider.subscribe_blocks().await },
                true,
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
    /// If `require_pubsub` is true, providers that don't support pubsub will be skipped.
    ///
    /// # Errors
    ///
    /// - Returns [`RpcError<TransportErrorKind>`] with message "total operation timeout exceeded
    ///   and all fallback providers failed" if the overall timeout elapses and no fallback
    ///   providers succeed.
    /// - Returns [`RpcError::Transport(TransportErrorKind::PubsubUnavailable)`] if `require_pubsub`
    ///   is true and all providers don't support pubsub.
    /// - Propagates any [`RpcError<TransportErrorKind>`] from the underlying retries.
    async fn retry_with_total_timeout<T: Debug, F, Fut>(
        &self,
        operation: F,
        require_pubsub: bool,
    ) -> Result<T, Error>
    where
        F: Fn(RootProvider<N>) -> Fut,
        Fut: Future<Output = Result<T, RpcError<TransportErrorKind>>>,
    {
        let mut skipped_count = 0;

        let mut providers = self.providers.iter();
        let primary = providers.next().expect("should have primary provider");

        let result = self.try_provider_with_timeout(primary, &operation).await;

        if result.is_ok() {
            return result;
        }

        let mut last_error = result.unwrap_err();

        if self.providers.len() > 1 {
            info!("Primary provider failed, trying fallback provider(s)");
        }

        // This loop starts at index 1 automatically
        for (idx, provider) in providers.enumerate() {
            let fallback_num = idx + 1;
            if require_pubsub && !Self::supports_pubsub(provider) {
                info!("Fallback provider {} doesn't support pubsub, skipping", fallback_num);
                skipped_count += 1;
                continue;
            }
            info!("Attempting fallback provider {}/{}", fallback_num, self.providers.len() - 1);

            match self.try_provider_with_timeout(provider, &operation).await {
                Ok(value) => {
                    info!(provider_num = fallback_num, "Fallback provider succeeded");
                    return Ok(value);
                }
                Err(e) => {
                    error!(provider_num = fallback_num, err = %e, "Fallback provider failed");
                    last_error = e;
                }
            }
        }

        // If all providers were skipped due to pubsub requirement
        if skipped_count == self.providers.len() {
            error!("All providers skipped - none support pubsub");
            return Err(RpcError::Transport(TransportErrorKind::PubsubUnavailable).into());
        }

        // Return the last error encountered
        error!("All providers failed or timed out");
        Err(last_error)
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
            .with_max_times(self.max_attempts)
            .with_min_delay(self.min_delay);
        println!("Retry strategy: {:?}", retry_strategy);

        timeout(
            self.max_timeout,
            (|| operation(provider.clone()))
                .retry(retry_strategy)
                .notify(|err: &RpcError<TransportErrorKind>, dur: Duration| {
                    println!("RPC error retrying after {:?}: {err:?}", dur);
                    // info!(error = %err, "RPC error retrying after {:?}", dur);
                })
                .sleep(tokio::time::sleep),
        )
        .await
        .map_err(Error::from)?
        .map_err(Error::from)
    }

    /// Check if a provider supports pubsub
    fn supports_pubsub(provider: &RootProvider<N>) -> bool {
        provider.client().pubsub_frontend().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::providers::{ProviderBuilder, WsConnect};
    use alloy_node_bindings::Anvil;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::time::sleep;

    fn test_provider(timeout: u64, max_retries: usize, min_delay: u64) -> RobustProvider {
        RobustProvider {
            providers: vec![RootProvider::new_http("http://localhost:8545".parse().unwrap())],
            max_timeout: Duration::from_millis(timeout),
            max_attempts: max_retries,
            min_delay: Duration::from_millis(min_delay),
        }
    }

    #[tokio::test]
    async fn test_retry_with_timeout_succeeds_on_first_attempt() {
        let provider = test_provider(100, 3, 10);

        let call_count = AtomicUsize::new(0);

        let result = provider
            .retry_with_total_timeout(
                |_| async {
                    call_count.fetch_add(1, Ordering::SeqCst);
                    let count = call_count.load(Ordering::SeqCst);
                    Ok(count)
                },
                false,
            )
            .await;

        assert!(matches!(result, Ok(1)));
    }

    #[tokio::test]
    async fn test_retry_with_timeout_retries_on_error() {
        let provider = test_provider(100, 3, 10);

        let call_count = AtomicUsize::new(0);

        let result = provider
            .retry_with_total_timeout(
                |_| async {
                    call_count.fetch_add(1, Ordering::SeqCst);
                    let count = call_count.load(Ordering::SeqCst);
                    match count {
                        3 => Ok(count),
                        _ => Err(TransportErrorKind::BackendGone.into()),
                    }
                },
                false,
            )
            .await;

        assert!(matches!(result, Ok(3)));
    }

    #[tokio::test]
    async fn test_retry_with_timeout_fails_after_max_retries() {
        let provider = test_provider(100, 2, 10);

        let call_count = AtomicUsize::new(0);

        let result: Result<(), Error> = provider
            .retry_with_total_timeout(
                |_| async {
                    call_count.fetch_add(1, Ordering::SeqCst);
                    Err(TransportErrorKind::BackendGone.into())
                },
                false,
            )
            .await;

        assert!(matches!(result, Err(Error::RpcError(_))));
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_retry_with_timeout_respects_max_timeout() {
        let max_timeout = 50;
        let provider = test_provider(max_timeout, 10, 1);

        let result = provider
            .retry_with_total_timeout(
                move |_provider| async move {
                    sleep(Duration::from_millis(max_timeout + 10)).await;
                    Ok(42)
                },
                false,
            )
            .await;

        assert!(matches!(result, Err(Error::Timeout)));
    }

    #[tokio::test]
    async fn test_subscribe_fails_causes_backup_to_be_used() {
        let anvil_1 = Anvil::new().port(2222_u16).try_spawn().expect("Failed to start anvil");

        let ws_provider_1 = ProviderBuilder::new()
            .connect_ws(WsConnect::new(anvil_1.ws_endpoint_url().as_str()))
            .await
            .expect("Failed to connect to WS");

        let anvil_2 = Anvil::new().port(1111_u16).try_spawn().expect("Failed to start anvil");

        let ws_provider_2 = ProviderBuilder::new()
            .connect_ws(WsConnect::new(anvil_2.ws_endpoint_url().as_str()))
            .await
            .expect("Failed to connect to WS");

        let robust = RobustProvider::no_retry(ws_provider_1)
            .fallback(ws_provider_2)
            .max_timeout(Duration::from_secs(5));

        drop(anvil_1);

        let result = robust.subscribe_blocks().await;

        assert!(result.is_ok(), "Expected subscribe blocks to work");
    }

    #[tokio::test]
    #[should_panic(expected = "called pubsub_frontend on a non-pubsub transport")]
    async fn test_subscribe_fails_if_primary_provider_lacks_pubsub() {
        let anvil = Anvil::new().try_spawn().expect("Failed to start anvil");

        let http_provider = ProviderBuilder::new().connect_http(anvil.endpoint_url());
        let ws_provider = ProviderBuilder::new()
            .connect_ws(WsConnect::new(anvil.ws_endpoint_url().as_str()))
            .await
            .expect("Failed to connect to WS");

        let robust = RobustProvider::no_retry(http_provider)
            .fallback(ws_provider)
            .max_timeout(Duration::from_secs(5));

        let _ = robust.subscribe_blocks().await;
    }

    #[tokio::test]
    async fn test_ws_fails_http_fallback_returns_primary_error() {
        let anvil_1 = Anvil::new().try_spawn().expect("Failed to start anvil");

        let ws_provider = ProviderBuilder::new()
            .connect_ws(WsConnect::new(anvil_1.ws_endpoint_url().as_str()))
            .await
            .expect("Failed to connect to WS");

        let anvil_2 = Anvil::new().port(8222_u16).try_spawn().expect("Failed to start anvil");
        let http_provider = ProviderBuilder::new().connect_http(anvil_2.endpoint_url());

        let robust = RobustProvider::no_retry(ws_provider.clone())
            .fallback(http_provider)
            .max_timeout(Duration::from_millis(500));

        // force ws_provider to fail and return BackendGone
        drop(anvil_1);

        let err = robust.subscribe_blocks().await.unwrap_err();

        // The error should be either a Timeout or BackendGone from the primary WS provider,
        // NOT a PubsubUnavailable error (which would indicate HTTP fallback was attempted)
        match err {
            Error::Timeout => {}
            Error::RpcError(e) => {
                assert!(matches!(e.as_ref(), RpcError::Transport(TransportErrorKind::BackendGone)));
            }
            Error::BlockNotFound(id) => panic!("Unexpected error type: BlockNotFound({id})"),
        }
    }
}
