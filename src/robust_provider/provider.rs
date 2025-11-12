use std::{fmt::Debug, time::Duration};

use alloy::{
    eips::BlockNumberOrTag,
    network::{Ethereum, Network},
    providers::{Provider, RootProvider},
    rpc::types::{Filter, Log},
    transports::{RpcError, TransportErrorKind},
};
use backon::{ExponentialBuilder, Retryable};
use tokio::time::timeout;
use tracing::{error, info};

use crate::{
    RobustSubscription,
    robust_provider::{Error, subscription::DEFAULT_RECONNECT_INTERVAL},
};

/// Provider wrapper with built-in retry and timeout mechanisms.
///
/// This wrapper around Alloy providers automatically handles retries,
/// timeouts, and error logging for RPC calls.
/// The first provider in the vector is treated as the primary provider.
#[derive(Clone, Debug)]
pub struct RobustProvider<N: Network = Ethereum> {
    pub(crate) providers: Vec<RootProvider<N>>,
    pub(crate) max_timeout: Duration,
    pub(crate) subscription_timeout: Duration,
    pub(crate) max_retries: usize,
    pub(crate) min_delay: Duration,
}

impl<N: Network> RobustProvider<N> {
    /// Get a reference to the primary provider (the first provider in the list)
    ///
    /// # Panics
    ///
    /// If there are no providers set (this should never happen)
    #[must_use]
    pub fn primary(&self) -> &RootProvider<N> {
        // Safe to unwrap because we always have at least one provider
        self.providers.first().expect("providers vector should never be empty")
    }

    /// Fetch a block by number with retry and timeout.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC call fails after exhausting all retry attempts
    /// or if the call times out. When fallback providers are configured, the error
    /// returned will be from the final provider that was attempted.
    pub async fn get_block_by_number(
        &self,
        number: BlockNumberOrTag,
    ) -> Result<N::BlockResponse, Error> {
        info!("eth_getBlockByNumber called");
        let result = self
            .try_operation_with_failover(
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
    /// Returns an error if the RPC call fails after exhausting all retry attempts
    /// or if the call times out. When fallback providers are configured, the error
    /// returned will be from the final provider that was attempted.
    pub async fn get_block_number(&self) -> Result<u64, Error> {
        info!("eth_getBlockNumber called");
        let result = self
            .try_operation_with_failover(
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
    /// Returns an error if the RPC call fails after exhausting all retry attempts
    /// or if the call times out. When fallback providers are configured, the error
    /// returned will be from the final provider that was attempted.
    pub async fn get_block_by_hash(
        &self,
        hash: alloy::primitives::BlockHash,
    ) -> Result<N::BlockResponse, Error> {
        info!("eth_getBlockByHash called");
        let result = self
            .try_operation_with_failover(
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
    /// Returns an error if the RPC call fails after exhausting all retry attempts
    /// or if the call times out. When fallback providers are configured, the error
    /// returned will be from the final provider that was attempted.
    pub async fn get_logs(&self, filter: &Filter) -> Result<Vec<Log>, Error> {
        info!("eth_getLogs called");
        let result = self
            .try_operation_with_failover(
                move |provider| async move { provider.get_logs(filter).await },
                false,
            )
            .await;
        if let Err(e) = &result {
            error!(error = %e, "eth_getLogs failed");
        }
        result
    }

    /// Subscribe to new block headers with automatic failover and reconnection.
    ///
    /// Returns a `RobustSubscription` that automatically:
    /// * Handles connection errors by switching to fallback providers
    /// * Detects and recovers from lagged subscriptions
    /// * Periodically attempts to reconnect to the primary provider
    ///
    /// # Errors
    ///
    /// Returns an error if the primary provider does not support pubsub, if the RPC
    /// call fails after exhausting all retry attempts, or if the call times out.
    /// When fallback providers are configured, the error returned will be from the
    /// final provider that was attempted.
    pub async fn subscribe_blocks(&self) -> Result<RobustSubscription<N>, Error> {
        info!("eth_subscribe called");
        let subscription = self
            .try_operation_with_failover(
                move |provider| async move { provider.subscribe_blocks().await },
                true,
            )
            .await;

        match subscription {
            Ok(sub) => Ok(RobustSubscription::new(sub, self.clone(), DEFAULT_RECONNECT_INTERVAL)),
            Err(e) => {
                error!(error = %e, "eth_subscribe failed");
                Err(e)
            }
        }
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
    /// * Returns [`RpcError<TransportErrorKind>`] with message "total operation timeout exceeded
    ///   and all fallback providers failed" if the overall timeout elapses and no fallback
    ///   providers succeed.
    /// * Returns [`RpcError::Transport(TransportErrorKind::PubsubUnavailable)`] if `require_pubsub`
    ///   is true and all providers don't support pubsub.
    /// * Propagates any [`RpcError<TransportErrorKind>`] from the underlying retries.
    pub(crate) async fn try_operation_with_failover<T: Debug, F, Fut>(
        &self,
        operation: F,
        require_pubsub: bool,
    ) -> Result<T, Error>
    where
        F: Fn(RootProvider<N>) -> Fut,
        Fut: Future<Output = Result<T, RpcError<TransportErrorKind>>>,
    {
        let primary = self.primary();
        let result = self.try_provider_with_timeout(primary, &operation).await;

        if result.is_ok() {
            return result;
        }

        let last_error = result.unwrap_err();

        self.try_fallback_providers(&operation, require_pubsub, last_error).await
    }

    pub(crate) async fn try_fallback_providers<T: Debug, F, Fut>(
        &self,
        operation: F,
        require_pubsub: bool,
        mut last_error: Error,
    ) -> Result<T, Error>
    where
        F: Fn(RootProvider<N>) -> Fut,
        Fut: Future<Output = Result<T, RpcError<TransportErrorKind>>>,
    {
        let num_providers = self.providers.len();
        if num_providers > 1 {
            info!("Primary provider failed, trying fallback provider(s)");
        }
        let fallback_providers = self.providers.iter().skip(1);
        for (idx, provider) in fallback_providers.enumerate() {
            let fallback_num = idx + 1;
            if require_pubsub && !Self::supports_pubsub(provider) {
                info!("Fallback provider {} doesn't support pubsub, skipping", fallback_num);
                continue;
            }
            info!("Attempting fallback provider {}/{}", fallback_num, num_providers - 1);

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
        // All fallbacks failed / skipped, return the last error
        error!("All providers failed or timed out - returning the last providers attempt's error");
        Err(last_error)
    }

    /// Try executing an operation with a specific provider with retry and timeout.
    pub(crate) async fn try_provider_with_timeout<T, F, Fut>(
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

    /// Check if a provider supports pubsub
    fn supports_pubsub(provider: &RootProvider<N>) -> bool {
        provider.client().pubsub_frontend().is_some()
    }
}

#[cfg(test)]
mod tests {
    use crate::robust_provider::{
        Error,
        builder::{DEFAULT_SUBSCRIPTION_TIMEOUT, RobustProviderBuilder},
    };

    use super::*;
    use alloy::{
        consensus::BlockHeader,
        providers::{ProviderBuilder, WsConnect, ext::AnvilApi},
        transports::{RpcError, TransportErrorKind},
    };
    use alloy_node_bindings::Anvil;
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    use tokio::time::sleep;
    use tokio_stream::StreamExt;

    fn test_provider(timeout: u64, max_retries: usize, min_delay: u64) -> RobustProvider {
        RobustProvider {
            providers: vec![RootProvider::new_http("http://localhost:8545".parse().unwrap())],
            max_timeout: Duration::from_millis(timeout),
            subscription_timeout: DEFAULT_SUBSCRIPTION_TIMEOUT,
            max_retries,
            min_delay: Duration::from_millis(min_delay),
        }
    }

    #[tokio::test]
    async fn test_retry_with_timeout_succeeds_on_first_attempt() {
        let provider = test_provider(100, 3, 10);

        let call_count = AtomicUsize::new(0);

        let result = provider
            .try_operation_with_failover(
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
            .try_operation_with_failover(
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
            .try_operation_with_failover(
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
            .try_operation_with_failover(
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
    async fn test_subscribe_fails_causes_backup_to_be_used() -> anyhow::Result<()> {
        let anvil_1 = Anvil::new().try_spawn()?;

        let ws_provider_1 =
            ProviderBuilder::new().connect(anvil_1.ws_endpoint_url().as_str()).await?;

        let anvil_2 = Anvil::new().try_spawn()?;

        let ws_provider_2 =
            ProviderBuilder::new().connect(anvil_2.ws_endpoint_url().as_str()).await?;

        let robust = RobustProviderBuilder::fragile(ws_provider_1.clone())
            .fallback(ws_provider_2.clone())
            .max_timeout(Duration::from_secs(1))
            .subscription_timeout(Duration::from_secs(1))
            .build()
            .await?;

        drop(anvil_1);

        let mut subscription = robust.subscribe_blocks().await?;

        ws_provider_2.anvil_mine(Some(2), None).await?;

        assert_eq!(1, subscription.recv().await?.number());
        assert_eq!(2, subscription.recv().await?.number());
        assert!(subscription.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn test_subscribe_fails_when_all_providers_lack_pubsub() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn()?;

        let http_provider = ProviderBuilder::new().connect_http(anvil.endpoint_url());

        let robust = RobustProviderBuilder::new(http_provider.clone())
            .fallback(http_provider)
            .max_timeout(Duration::from_secs(5))
            .min_delay(Duration::from_millis(100))
            .build()
            .await?;

        let result = robust.subscribe_blocks().await.unwrap_err();

        match result {
            Error::RpcError(e) => {
                assert!(matches!(
                    e.as_ref(),
                    RpcError::Transport(TransportErrorKind::PubsubUnavailable)
                ));
            }
            other => panic!("Expected PubsubUnavailable error type, got: {other:?}"),
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_subscribe_succeeds_if_primary_provider_lacks_pubsub_but_fallback_supports_it()
    -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn()?;

        let http_provider = ProviderBuilder::new().connect_http(anvil.endpoint_url());
        let ws_provider = ProviderBuilder::new()
            .connect_ws(WsConnect::new(anvil.ws_endpoint_url().as_str()))
            .await?;

        let robust = RobustProviderBuilder::fragile(http_provider)
            .fallback(ws_provider)
            .max_timeout(Duration::from_secs(5))
            .build()
            .await?;

        let result = robust.subscribe_blocks().await;
        assert!(result.is_ok());

        Ok(())
    }

    #[tokio::test]
    async fn test_ws_fails_http_fallback_returns_primary_error() -> anyhow::Result<()> {
        let anvil_1 = Anvil::new().try_spawn()?;

        let ws_provider =
            ProviderBuilder::new().connect(anvil_1.ws_endpoint_url().as_str()).await?;

        let anvil_2 = Anvil::new().try_spawn()?;
        let http_provider = ProviderBuilder::new().connect_http(anvil_2.endpoint_url());

        let robust = RobustProviderBuilder::fragile(ws_provider.clone())
            .fallback(http_provider)
            .max_timeout(Duration::from_millis(500))
            .subscription_timeout(Duration::from_secs(1))
            .build()
            .await?;

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

        Ok(())
    }

    #[tokio::test]
    async fn test_stream_with_failover() -> anyhow::Result<()> {
        let mut anvil_1 = Some(Anvil::new().block_time_f64(0.1).try_spawn()?);

        let ws_provider = ProviderBuilder::new()
            .connect(anvil_1.as_ref().unwrap().ws_endpoint_url().as_str())
            .await?;

        let anvil_2 = Anvil::new().block_time_f64(0.1).try_spawn()?;

        let ws_provider_2 =
            ProviderBuilder::new().connect(anvil_2.ws_endpoint_url().as_str()).await?;

        let robust = RobustProviderBuilder::fragile(ws_provider.clone())
            .fallback(ws_provider_2)
            .subscription_timeout(Duration::from_millis(500))
            .build()
            .await?;

        let subscription = robust.subscribe_blocks().await?;
        let mut stream = subscription.into_stream();

        while let Some(result) = stream.next().await {
            let Ok(block) = result else {
                break;
            };

            let block_number = block.number();

            // At block 10, drop the primary provider to test failover
            if block_number == 10 &&
                let Some(anvil) = anvil_1.take()
            {
                drop(anvil);
            }

            if block_number >= 11 {
                break;
            }
        }

        Ok(())
    }
}
