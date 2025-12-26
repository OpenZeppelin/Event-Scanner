use std::{fmt::Debug, sync::Arc, time::Duration};

use alloy::{
    eips::{BlockId, BlockNumberOrTag},
    network::{Ethereum, Network},
    primitives::{BlockHash, BlockNumber},
    providers::{Provider, RootProvider},
    rpc::types::{Filter, Log},
    transports::{RpcError, TransportErrorKind},
};
use backon::{ExponentialBuilder, Retryable};
use futures::TryFutureExt;
use thiserror::Error;
use tokio::time::{error as TokioError, timeout};

use crate::robust_provider::RobustSubscription;

/// Errors that can occur when using [`RobustProvider`].
#[derive(Error, Debug, Clone)]
pub enum Error {
    #[error("Operation timed out")]
    Timeout,
    #[error("RPC call failed after exhausting all retry attempts: {0}")]
    RpcError(Arc<RpcError<TransportErrorKind>>),
    #[error("Block not found, Block Id: {0}")]
    BlockNotFound(BlockId),
}

/// Low-level error related to RPC calls and failover logic.
#[derive(Error, Debug)]
pub enum CoreError {
    #[error("Operation timed out")]
    Timeout,
    #[error("RPC call failed after exhausting all retry attempts: {0}")]
    RpcError(RpcError<TransportErrorKind>),
}

impl From<RpcError<TransportErrorKind>> for CoreError {
    fn from(err: RpcError<TransportErrorKind>) -> Self {
        CoreError::RpcError(err)
    }
}

impl From<CoreError> for Error {
    fn from(err: CoreError) -> Self {
        match err {
            CoreError::Timeout => Error::Timeout,
            CoreError::RpcError(e) => Error::RpcError(Arc::new(e)),
        }
    }
}

impl From<TokioError::Elapsed> for CoreError {
    fn from(_: TokioError::Elapsed) -> Self {
        CoreError::Timeout
    }
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

impl From<super::subscription::Error> for Error {
    fn from(err: super::subscription::Error) -> Self {
        match err {
            super::subscription::Error::Timeout => Error::Timeout,
            super::subscription::Error::RpcError(e) => Error::RpcError(e),
            super::subscription::Error::Closed | super::subscription::Error::Lagged(_) => {
                Error::Timeout
            }
        }
    }
}

/// Provider wrapper with built-in retry and timeout mechanisms.
///
/// This wrapper around Alloy providers automatically handles retries,
/// timeouts, and error logging for RPC calls.
#[derive(Clone, Debug)]
pub struct RobustProvider<N: Network = Ethereum> {
    pub(crate) primary_provider: RootProvider<N>,
    pub(crate) fallback_providers: Vec<RootProvider<N>>,
    pub(crate) call_timeout: Duration,
    pub(crate) subscription_timeout: Duration,
    pub(crate) max_retries: usize,
    pub(crate) min_delay: Duration,
    pub(crate) reconnect_interval: Duration,
    pub(crate) subscription_buffer_capacity: usize,
}

impl<N: Network> RobustProvider<N> {
    /// Get a reference to the primary provider
    #[must_use]
    pub fn primary(&self) -> &RootProvider<N> {
        &self.primary_provider
    }

    /// Fetch a block by [`BlockNumberOrTag`] with retry and timeout.
    ///
    /// This is a wrapper function for [`Provider::get_block_by_number`].
    ///
    /// # Errors
    ///
    /// * [`Error::RpcError`] - if no fallback providers succeeded; contains the last error returned
    ///   by the last provider attempted on the last retry.
    /// * [`Error::Timeout`] - if the overall operation timeout elapses (i.e. exceeds
    ///   `call_timeout`).
    /// * [`Error::BlockNotFound`] - if the block with the specified hash was not found on-chain.
    pub async fn get_block_by_number(
        &self,
        number: BlockNumberOrTag,
    ) -> Result<N::BlockResponse, Error> {
        let result = self
            .try_operation_with_failover(
                move |provider| async move { provider.get_block_by_number(number).await },
                false,
            )
            .await;

        result?.ok_or_else(|| Error::BlockNotFound(number.into()))
    }

    /// Fetch a block number by [`BlockId`]  with retry and timeout.
    ///
    /// This is a wrapper function for [`Provider::get_block`].
    ///
    /// # Errors
    ///
    /// * [`Error::RpcError`] - if no fallback providers succeeded; contains the last error returned
    ///   by the last provider attempted on the last retry.
    /// * [`Error::Timeout`] - if the overall operation timeout elapses (i.e. exceeds
    ///   `call_timeout`).
    /// * [`Error::BlockNotFound`] - if the block with the specified hash was not found on-chain.
    pub async fn get_block(&self, id: BlockId) -> Result<N::BlockResponse, Error> {
        let result = self
            .try_operation_with_failover(
                |provider| async move { provider.get_block(id).await },
                false,
            )
            .await;
        result?.ok_or_else(|| Error::BlockNotFound(id))
    }

    /// Fetch the latest block number with retry and timeout.
    ///
    /// This is a wrapper function for [`Provider::get_block_number`].
    ///
    /// # Errors
    ///
    /// * [`Error::RpcError`] - if no fallback providers succeeded; contains the last error returned
    ///   by the last provider attempted on the last retry.
    /// * [`Error::Timeout`] - if the overall operation timeout elapses (i.e. exceeds
    ///   `call_timeout`).
    pub async fn get_block_number(&self) -> Result<BlockNumber, Error> {
        self.try_operation_with_failover(
            move |provider| async move { provider.get_block_number().await },
            false,
        )
        .await
        .map_err(Error::from)
    }

    /// Get the block number for a given block identifier.
    ///
    /// This is a wrapper function for [`Provider::get_block_number_by_id`].
    ///
    /// # Arguments
    ///
    /// * `block_id` - The block identifier to fetch the block number for.
    ///
    /// # Errors
    ///
    /// * [`Error::RpcError`] - if no fallback providers succeeded; contains the last error returned
    ///   by the last provider attempted on the last retry.
    /// * [`Error::Timeout`] - if the overall operation timeout elapses (i.e. exceeds
    ///   `call_timeout`).
    /// * [`Error::BlockNotFound`] - if the block with the specified hash was not found on-chain.
    pub async fn get_block_number_by_id(&self, block_id: BlockId) -> Result<BlockNumber, Error> {
        let result = self
            .try_operation_with_failover(
                move |provider| async move { provider.get_block_number_by_id(block_id).await },
                false,
            )
            .await;
        result?.ok_or_else(|| Error::BlockNotFound(block_id))
    }

    /// Fetch the latest confirmed block number with retry and timeout.
    ///
    /// This method fetches the latest block number and subtracts the specified
    /// number of confirmations to get a "confirmed" block number.
    ///
    /// # Arguments
    ///
    /// * `confirmations` - The number of block confirmations to wait for. The returned block number
    ///   will be `latest_block - confirmations`.
    ///
    /// # Errors
    ///
    /// * [`Error::RpcError`] - if no fallback providers succeeded; contains the last error returned
    ///   by the last provider attempted on the last retry.
    /// * [`Error::Timeout`] - if the overall operation timeout elapses (i.e. exceeds
    ///   `call_timeout`).
    pub async fn get_latest_confirmed(&self, confirmations: u64) -> Result<u64, Error> {
        let latest_block = self.get_block_number().await?;
        let confirmed_block = latest_block.saturating_sub(confirmations);
        Ok(confirmed_block)
    }

    /// Fetch a block by [`BlockHash`] with retry and timeout.
    ///
    /// This is a wrapper function for [`Provider::get_block_by_hash`].
    ///
    /// # Errors
    ///
    /// * [`Error::RpcError`] - if no fallback providers succeeded; contains the last error returned
    ///   by the last provider attempted on the last retry.
    /// * [`Error::Timeout`] - if the overall operation timeout elapses (i.e. exceeds
    ///   `call_timeout`).
    /// * [`Error::BlockNotFound`] - if the block with the specified hash was not found on-chain.
    pub async fn get_block_by_hash(&self, hash: BlockHash) -> Result<N::BlockResponse, Error> {
        let result = self
            .try_operation_with_failover(
                move |provider| async move { provider.get_block_by_hash(hash).await },
                false,
            )
            .await;

        result?.ok_or_else(|| Error::BlockNotFound(hash.into()))
    }

    /// Fetch logs for the given [`Filter`] with retry and timeout.
    ///
    /// This is a wrapper function for [`Provider::get_logs`].
    ///
    /// # Errors
    ///
    /// * [`Error::RpcError`] - if no fallback providers succeeded; contains the last error returned
    ///   by the last provider attempted on the last retry.
    /// * [`Error::Timeout`] - if the overall operation timeout elapses (i.e. exceeds
    ///   `call_timeout`).
    pub async fn get_logs(&self, filter: &Filter) -> Result<Vec<Log>, Error> {
        self.try_operation_with_failover(
            move |provider| async move { provider.get_logs(filter).await },
            false,
        )
        .await
        .map_err(Error::from)
    }

    /// Subscribe to new block headers with automatic failover and reconnection.
    ///
    /// Returns a `RobustSubscription` that automatically:
    /// * Handles connection errors by switching to fallback providers
    /// * Detects and recovers from lagged subscriptions
    /// * Periodically attempts to reconnect to the primary provider
    ///
    /// This is a wrapper function for [`Provider::subscribe_blocks`].
    ///
    /// # Errors
    ///
    /// * [`Error::RpcError`] - if no fallback providers succeeded; contains the last error returned
    ///   by the last provider attempted on the last retry.
    /// * [`Error::Timeout`] - if the overall operation timeout elapses (i.e. exceeds
    ///   `call_timeout`).
    pub async fn subscribe_blocks(&self) -> Result<RobustSubscription<N>, Error> {
        let subscription = self
            .try_operation_with_failover(
                move |provider| async move {
                    provider
                        .subscribe_blocks()
                        .channel_size(self.subscription_buffer_capacity)
                        .await
                },
                true,
            )
            .await?;

        Ok(RobustSubscription::new(subscription, self.clone()))
    }

    /// Execute `operation` with exponential backoff and a total timeout.
    ///
    /// Wraps the retry logic with [`tokio::time::timeout`] so
    /// the entire operation (including time spent inside the RPC call) cannot exceed
    /// `call_timeout`.
    ///
    /// If the timeout is exceeded and fallback providers are available, it will
    /// attempt to use each fallback provider in sequence.
    ///
    /// If `require_pubsub` is true, providers that don't support pubsub will be skipped.
    ///
    /// # Errors
    ///
    /// * [`CoreError::RpcError`] - if no fallback providers succeeded; contains the last error
    ///   returned by the last provider attempted on the last retry.
    /// * [`CoreError::Timeout`] - if the overall operation timeout elapses (i.e. exceeds
    ///   `call_timeout`).
    pub async fn try_operation_with_failover<T: Debug, F, Fut>(
        &self,
        operation: F,
        require_pubsub: bool,
    ) -> Result<T, CoreError>
    where
        F: Fn(RootProvider<N>) -> Fut,
        Fut: Future<Output = Result<T, RpcError<TransportErrorKind>>>,
    {
        let primary = self.primary();
        self.try_provider_with_timeout(primary, &operation)
            .or_else(|last_error| {
                self.try_fallback_providers_from(&operation, require_pubsub, last_error, 0)
                    .map_ok(|(value, _)| value)
            })
            .await
    }

    pub(crate) async fn try_fallback_providers_from<T: Debug, F, Fut>(
        &self,
        operation: F,
        require_pubsub: bool,
        mut last_error: CoreError,
        start_index: usize,
    ) -> Result<(T, usize), CoreError>
    where
        F: Fn(RootProvider<N>) -> Fut,
        Fut: Future<Output = Result<T, RpcError<TransportErrorKind>>>,
    {
        let num_fallbacks = self.fallback_providers.len();

        debug!(
            start_index = start_index,
            total_fallbacks = num_fallbacks,
            require_pubsub = require_pubsub,
            "Primary provider failed, attempting fallback providers"
        );

        let fallback_providers = self.fallback_providers.iter().enumerate().skip(start_index);
        for (fallback_idx, provider) in fallback_providers {
            if require_pubsub && !Self::supports_pubsub(provider) {
                debug!(
                    provider_index = fallback_idx,
                    "Skipping fallback provider: pubsub not supported"
                );
                continue;
            }

            trace!(
                fallback_index = fallback_idx,
                total_fallbacks = num_fallbacks,
                "Attempting fallback provider"
            );

            match self.try_provider_with_timeout(provider, &operation).await {
                Ok(value) => {
                    info!(
                        fallback_index = fallback_idx,
                        total_fallbacks = num_fallbacks,
                        "Switched to fallback provider"
                    );
                    return Ok((value, fallback_idx));
                }
                Err(e) => {
                    warn!(
                        fallback_index = fallback_idx,
                        error = %e,
                        "Fallback provider failed"
                    );
                    last_error = e;
                }
            }
        }

        error!(attempted_providers = num_fallbacks + 1, "All providers exhausted");

        Err(last_error)
    }

    /// Try executing an operation with a specific provider with retry and timeout.
    pub(crate) async fn try_provider_with_timeout<T, F, Fut>(
        &self,
        provider: &RootProvider<N>,
        operation: F,
    ) -> Result<T, CoreError>
    where
        F: Fn(RootProvider<N>) -> Fut,
        Fut: Future<Output = Result<T, RpcError<TransportErrorKind>>>,
    {
        let retry_strategy = ExponentialBuilder::default()
            .with_max_times(self.max_retries)
            .with_min_delay(self.min_delay);

        timeout(
            self.call_timeout,
            (|| operation(provider.clone())).retry(retry_strategy).sleep(tokio::time::sleep),
        )
        .await
        .map_err(CoreError::from)?
        .map_err(CoreError::from)
    }

    /// Check if a provider supports pubsub
    fn supports_pubsub(provider: &RootProvider<N>) -> bool {
        provider.client().pubsub_frontend().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::robust_provider::{
        DEFAULT_SUBSCRIPTION_BUFFER_CAPACITY, RobustProviderBuilder,
        builder::DEFAULT_SUBSCRIPTION_TIMEOUT, subscription::DEFAULT_RECONNECT_INTERVAL,
    };
    use alloy::providers::{ProviderBuilder, WsConnect, ext::AnvilApi};
    use alloy_node_bindings::{Anvil, AnvilInstance};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::time::sleep;

    async fn setup_anvil() -> anyhow::Result<(AnvilInstance, RobustProvider, impl Provider)> {
        let anvil = Anvil::new().try_spawn()?;
        let alloy_provider = ProviderBuilder::new().connect_http(anvil.endpoint_url());

        let robust = RobustProviderBuilder::new(alloy_provider.clone())
            .call_timeout(Duration::from_secs(5))
            .build()
            .await?;

        Ok((anvil, robust, alloy_provider))
    }

    async fn setup_anvil_with_blocks(
        num_blocks: u64,
    ) -> anyhow::Result<(AnvilInstance, RobustProvider, impl Provider)> {
        let (anvil, robust, alloy_provider) = setup_anvil().await?;
        alloy_provider.anvil_mine(Some(num_blocks), None).await?;
        Ok((anvil, robust, alloy_provider))
    }

    fn test_provider(timeout: u64, max_retries: usize, min_delay: u64) -> RobustProvider {
        RobustProvider {
            primary_provider: RootProvider::new_http("http://localhost:8545".parse().unwrap()),
            fallback_providers: vec![],
            call_timeout: Duration::from_millis(timeout),
            subscription_timeout: DEFAULT_SUBSCRIPTION_TIMEOUT,
            max_retries,
            min_delay: Duration::from_millis(min_delay),
            reconnect_interval: DEFAULT_RECONNECT_INTERVAL,
            subscription_buffer_capacity: DEFAULT_SUBSCRIPTION_BUFFER_CAPACITY,
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

        let result: Result<(), CoreError> = provider
            .try_operation_with_failover(
                |_| async {
                    call_count.fetch_add(1, Ordering::SeqCst);
                    Err(TransportErrorKind::BackendGone.into())
                },
                false,
            )
            .await;

        assert!(matches!(result, Err(CoreError::RpcError(_))));
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_retry_with_timeout_respects_call_timeout() {
        let call_timeout = 50;
        let provider = test_provider(call_timeout, 10, 1);

        let result = provider
            .try_operation_with_failover(
                move |_provider| async move {
                    sleep(Duration::from_millis(call_timeout + 10)).await;
                    Ok(42)
                },
                false,
            )
            .await;

        assert!(matches!(result, Err(CoreError::Timeout)));
    }

    #[tokio::test]
    async fn test_subscribe_fails_when_all_providers_lack_pubsub() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn()?;

        let http_provider = ProviderBuilder::new().connect_http(anvil.endpoint_url());

        let robust = RobustProviderBuilder::new(http_provider.clone())
            .fallback(http_provider)
            .call_timeout(Duration::from_secs(5))
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
            .call_timeout(Duration::from_secs(5))
            .build()
            .await?;

        let result = robust.subscribe_blocks().await;
        assert!(result.is_ok());

        Ok(())
    }

    #[tokio::test]
    async fn test_get_block_by_number_succeeds() -> anyhow::Result<()> {
        let (_anvil, robust, alloy_provider) = setup_anvil_with_blocks(100).await?;

        let tags = [
            BlockNumberOrTag::Number(50),
            BlockNumberOrTag::Latest,
            BlockNumberOrTag::Earliest,
            BlockNumberOrTag::Safe,
            BlockNumberOrTag::Finalized,
        ];

        for tag in tags {
            let robust_block = robust.get_block_by_number(tag).await?;
            let alloy_block =
                alloy_provider.get_block_by_number(tag).await?.expect("block should exist");

            assert_eq!(robust_block.header.number, alloy_block.header.number);
            assert_eq!(robust_block.header.hash, alloy_block.header.hash);
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_get_block_by_number_future_block_fails() -> anyhow::Result<()> {
        let (_anvil, robust, _alloy_provider) = setup_anvil().await?;

        let future_block = 999_999;
        let result = robust.get_block_by_number(BlockNumberOrTag::Number(future_block)).await;

        assert!(matches!(result, Err(Error::BlockNotFound(_))));

        Ok(())
    }

    #[tokio::test]
    async fn test_get_block_succeeds() -> anyhow::Result<()> {
        let (_anvil, robust, alloy_provider) = setup_anvil_with_blocks(100).await?;

        let block_ids = [
            BlockId::number(50),
            BlockId::latest(),
            BlockId::earliest(),
            BlockId::safe(),
            BlockId::finalized(),
        ];

        for block_id in block_ids {
            let robust_block = robust.get_block(block_id).await?;
            let alloy_block =
                alloy_provider.get_block(block_id).await?.expect("block should exist");

            assert_eq!(robust_block.header.number, alloy_block.header.number);
            assert_eq!(robust_block.header.hash, alloy_block.header.hash);
        }

        // test block hash
        let block = alloy_provider
            .get_block_by_number(BlockNumberOrTag::Number(50))
            .await?
            .expect("block should exist");
        let block_hash = block.header.hash;
        let block_id = BlockId::hash(block_hash);
        let robust_block = robust.get_block(block_id).await?;
        assert_eq!(robust_block.header.hash, block_hash);
        assert_eq!(robust_block.header.number, 50);

        Ok(())
    }

    #[tokio::test]
    async fn test_get_block_fails() -> anyhow::Result<()> {
        let (_anvil, robust, _alloy_provider) = setup_anvil().await?;

        // Future block number
        let result = robust.get_block(BlockId::number(999_999)).await;
        assert!(matches!(result, Err(Error::BlockNotFound(_))));

        // Non-existent hash
        let result = robust.get_block(BlockId::hash(BlockHash::ZERO)).await;
        assert!(matches!(result, Err(Error::BlockNotFound(_))));

        Ok(())
    }

    #[tokio::test]
    async fn test_get_block_number_succeeds() -> anyhow::Result<()> {
        let (_anvil, robust, alloy_provider) = setup_anvil_with_blocks(100).await?;

        let robust_block_num = robust.get_block_number().await?;
        let alloy_block_num = alloy_provider.get_block_number().await?;
        assert_eq!(robust_block_num, alloy_block_num);
        assert_eq!(robust_block_num, 100);

        alloy_provider.anvil_mine(Some(10), None).await?;
        let new_block = robust.get_block_number().await?;
        assert_eq!(new_block, 110);

        Ok(())
    }

    #[tokio::test]
    async fn test_get_block_number_by_id_succeeds() -> anyhow::Result<()> {
        let (_anvil, robust, alloy_provider) = setup_anvil_with_blocks(100).await?;

        let block_num = robust.get_block_number_by_id(BlockId::number(50)).await?;
        assert_eq!(block_num, 50);

        let block = alloy_provider
            .get_block_by_number(BlockNumberOrTag::Number(50))
            .await?
            .expect("block should exist");
        let block_num = robust.get_block_number_by_id(BlockId::hash(block.header.hash)).await?;
        assert_eq!(block_num, 50);

        let block_num = robust.get_block_number_by_id(BlockId::latest()).await?;
        assert_eq!(block_num, 100);

        let block_num = robust.get_block_number_by_id(BlockId::earliest()).await?;
        assert_eq!(block_num, 0);

        // Returns block number even if it doesnt 'exist' on chain
        let block_num = robust.get_block_number_by_id(BlockId::number(999_999)).await?;
        let alloy_block_num = alloy_provider
            .get_block_number_by_id(BlockId::number(999_999))
            .await?
            .expect("Should return block num");
        assert_eq!(alloy_block_num, block_num);
        assert_eq!(block_num, 999_999);

        Ok(())
    }

    #[tokio::test]
    async fn test_get_block_number_by_id_fails() -> anyhow::Result<()> {
        let (_anvil, robust, _alloy_provider) = setup_anvil().await?;

        let result = robust.get_block_number_by_id(BlockId::hash(BlockHash::ZERO)).await;
        assert!(matches!(result, Err(Error::BlockNotFound(_))));

        Ok(())
    }

    #[tokio::test]
    async fn test_get_latest_confirmed_succeeds() -> anyhow::Result<()> {
        let (_anvil, robust, _alloy_provider) = setup_anvil_with_blocks(100).await?;

        // With confirmations
        let confirmed_block = robust.get_latest_confirmed(10).await?;
        assert_eq!(confirmed_block, 90);

        // Zero confirmations returns latest
        let confirmed_block = robust.get_latest_confirmed(0).await?;
        assert_eq!(confirmed_block, 100);

        // Single confirmation
        let confirmed_block = robust.get_latest_confirmed(1).await?;
        assert_eq!(confirmed_block, 99);

        // confirmations = latest - 1
        let confirmed_block = robust.get_latest_confirmed(99).await?;
        assert_eq!(confirmed_block, 1);

        // confirmations = latest (should return 0)
        let confirmed_block = robust.get_latest_confirmed(100).await?;
        assert_eq!(confirmed_block, 0);

        // confirmations = latest + 1 (saturates at zero)
        let confirmed_block = robust.get_latest_confirmed(101).await?;
        assert_eq!(confirmed_block, 0);

        // Saturates at zero when confirmations > latest
        let confirmed_block = robust.get_latest_confirmed(200).await?;
        assert_eq!(confirmed_block, 0);

        Ok(())
    }

    #[tokio::test]
    async fn test_get_block_by_hash_succeeds() -> anyhow::Result<()> {
        let (_anvil, robust, alloy_provider) = setup_anvil_with_blocks(100).await?;

        let block = alloy_provider
            .get_block_by_number(BlockNumberOrTag::Number(50))
            .await?
            .expect("block should exist");
        let block_hash = block.header.hash;

        let robust_block = robust.get_block_by_hash(block_hash).await?;
        let alloy_block =
            alloy_provider.get_block_by_hash(block_hash).await?.expect("block should exist");
        assert_eq!(robust_block.header.hash, alloy_block.header.hash);
        assert_eq!(robust_block.header.number, alloy_block.header.number);

        let genesis = alloy_provider
            .get_block_by_number(BlockNumberOrTag::Earliest)
            .await?
            .expect("genesis should exist");
        let genesis_hash = genesis.header.hash;
        let robust_block = robust.get_block_by_hash(genesis_hash).await?;
        assert_eq!(robust_block.header.number, 0);
        assert_eq!(robust_block.header.hash, genesis_hash);

        Ok(())
    }

    #[tokio::test]
    async fn test_get_block_by_hash_fails() -> anyhow::Result<()> {
        let (_anvil, robust, _alloy_provider) = setup_anvil().await?;

        let result = robust.get_block_by_hash(BlockHash::ZERO).await;
        assert!(matches!(result, Err(Error::BlockNotFound(_))));

        Ok(())
    }
}
