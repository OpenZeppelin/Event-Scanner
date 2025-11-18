use std::{fmt::Debug, time::Duration};

use alloy::{
    self,
    eips::BlockNumberOrTag,
    network::{Ethereum, Network},
    providers::{Provider, RootProvider},
    rpc::types::{Filter, Log},
    transports::{RpcError, TransportErrorKind},
};
use backon::{ExponentialBuilder, Retryable};
use tokio::time::timeout;
use tracing::{error, info};

use crate::robust_provider::{Error, RobustSubscription};

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
}

impl<N: Network> RobustProvider<N> {
    /// Get a reference to the primary provider
    #[must_use]
    pub fn primary(&self) -> &RootProvider<N> {
        &self.primary_provider
    }

    /// Fetch a block by number with retry and timeout.
    ///
    /// # Errors
    ///
    /// See [retry errors](#retry-errors).
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
    /// See [retry errors](#retry-errors).
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
    /// See [retry errors](#retry-errors).
    pub async fn get_latest_confirmed(&self, confirmations: u64) -> Result<u64, Error> {
        info!("get_latest_confirmed called with confirmations={}", confirmations);
        let latest_block = self.get_block_number().await?;
        let confirmed_block = latest_block.saturating_sub(confirmations);
        Ok(confirmed_block)
    }

    /// Fetch a block by hash with retry and timeout.
    ///
    /// # Errors
    ///
    /// See [retry errors](#retry-errors).
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
    /// See [retry errors](#retry-errors).
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
    /// see [retry errors](#retry-errors).
    pub async fn subscribe_blocks(&self) -> Result<RobustSubscription<N>, Error> {
        info!("eth_subscribe called");
        let subscription = self
            .try_operation_with_failover(
                move |provider| async move { provider.subscribe_blocks().await },
                true,
            )
            .await;

        match subscription {
            Ok(sub) => Ok(RobustSubscription::new(sub, self.clone())),
            Err(e) => {
                error!(error = %e, "eth_subscribe failed");
                Err(e)
            }
        }
    }

    /// Execute `operation` with exponential backoff and a total timeout.
    ///
    /// Wraps the retry logic with `tokio::time::timeout(self.call_timeout, ...)` so
    /// the entire operation (including time spent inside the RPC call) cannot exceed
    /// `call_timeout`.
    ///
    /// If the timeout is exceeded and fallback providers are available, it will
    /// attempt to use each fallback provider in sequence.
    ///
    /// If `require_pubsub` is true, providers that don't support pubsub will be skipped.
    ///
    /// # Errors
    /// <a name="retry-errors"></a>
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
        last_error: Error,
    ) -> Result<T, Error>
    where
        F: Fn(RootProvider<N>) -> Fut,
        Fut: Future<Output = Result<T, RpcError<TransportErrorKind>>>,
    {
        self.try_fallback_providers_from(operation, require_pubsub, last_error, 0)
            .await
            .map(|(value, _idx)| value)
    }

    pub(crate) async fn try_fallback_providers_from<T: Debug, F, Fut>(
        &self,
        operation: F,
        require_pubsub: bool,
        mut last_error: Error,
        start_index: usize,
    ) -> Result<(T, usize), Error>
    where
        F: Fn(RootProvider<N>) -> Fut,
        Fut: Future<Output = Result<T, RpcError<TransportErrorKind>>>,
    {
        let num_fallbacks = self.fallback_providers.len();
        if num_fallbacks > 0 && start_index == 0 {
            info!("Primary provider failed, trying fallback provider(s)");
        }

        let fallback_providers = self.fallback_providers.iter().enumerate().skip(start_index);
        for (fallback_idx, provider) in fallback_providers {
            if require_pubsub && !Self::supports_pubsub(provider) {
                info!("Fallback provider {} doesn't support pubsub, skipping", fallback_idx + 1);
                continue;
            }
            info!("Attempting fallback provider {}/{}", fallback_idx + 1, num_fallbacks);

            match self.try_provider_with_timeout(provider, &operation).await {
                Ok(value) => {
                    info!(provider_num = fallback_idx + 1, "Fallback provider succeeded");
                    return Ok((value, fallback_idx));
                }
                Err(e) => {
                    error!(provider_num = fallback_idx + 1, err = %e, "Fallback provider failed");
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
            self.call_timeout,
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
    use super::*;
    use crate::robust_provider::{
        RobustProviderBuilder,
        builder::DEFAULT_SUBSCRIPTION_TIMEOUT,
        subscription::{DEFAULT_RECONNECT_INTERVAL, RobustSubscriptionStream},
    };
    use alloy::providers::{ProviderBuilder, WsConnect, ext::AnvilApi};
    use alloy_node_bindings::{Anvil, AnvilInstance};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::time::sleep;
    use tokio_stream::StreamExt;

    fn test_provider(timeout: u64, max_retries: usize, min_delay: u64) -> RobustProvider {
        RobustProvider {
            primary_provider: RootProvider::new_http("http://localhost:8545".parse().unwrap()),
            fallback_providers: vec![],
            call_timeout: Duration::from_millis(timeout),
            subscription_timeout: DEFAULT_SUBSCRIPTION_TIMEOUT,
            max_retries,
            min_delay: Duration::from_millis(min_delay),
            reconnect_interval: DEFAULT_RECONNECT_INTERVAL,
        }
    }

    const SHORT_TIMEOUT: Duration = Duration::from_millis(300);
    const RECONNECT_INTERVAL: Duration = Duration::from_millis(500);
    const BUFFER_TIME: Duration = Duration::from_millis(100);

    async fn spawn_ws_anvil() -> anyhow::Result<(AnvilInstance, RootProvider)> {
        let anvil = Anvil::new().try_spawn()?;
        let provider = ProviderBuilder::new().connect(anvil.ws_endpoint_url().as_str()).await?;
        Ok((anvil, provider.root().to_owned()))
    }

    async fn mine_after_delay(provider: RootProvider, blocks: u64, delay: Duration) {
        sleep(delay).await;
        provider.anvil_mine(Some(blocks), None).await.unwrap();
    }

    fn assert_backend_gone_or_timeout(err: Error) {
        match err {
            Error::Timeout => {}
            Error::RpcError(e) => {
                assert!(
                    matches!(e.as_ref(), RpcError::Transport(TransportErrorKind::BackendGone)),
                    "Expected BackendGone error, got: {e:?}",
                );
            }
            Error::BlockNotFound(_) => {
                panic!("Unexpected BlockNotFound error");
            }
        }
    }

    #[macro_export]
    macro_rules! assert_stream_finished {
        ($stream: expr) => {
            $crate::assert_stream_finished!($stream, finish_secs = 3)
        };
        ($stream: expr, finish_secs = $finish: expr) => {{
            let next_item = tokio_stream::StreamExt::next(&mut $stream).await;
            match next_item {
                Some(Ok(item)) => panic!("Expected no item during quiet window, got: {:?}", item),
                None => {}
                Some(Err(e)) => {
                    assert!(matches!(e, Error::Timeout), "Expected Timeout error, got: {:?}", e);

                    let second = tokio::time::timeout(
                        std::time::Duration::from_secs($finish),
                        tokio_stream::StreamExt::next(&mut $stream),
                    )
                    .await
                    .expect("expected stream to finish after quiet window");
                    assert!(second.is_none(), "Expected stream to be finished, got: {:?}", second);
                }
            }
        }};
    }

    #[macro_export]
    macro_rules! assert_next_block {
        ($stream: expr, $expected: expr) => {
            assert_next_block!($stream, $expected, timeout = 5)
        };
        ($stream: expr, $expected: expr, timeout = $secs: expr) => {
            let message = tokio::time::timeout(
                std::time::Duration::from_secs($secs),
                tokio_stream::StreamExt::next(&mut $stream),
            )
            .await
            .expect("timed out");
            if let Some(block) = message {
                match block {
                    Ok(block) => assert_eq!(block.number, $expected),
                    Err(e) => panic!("Got err {e:?}"),
                }
            } else {
                panic!("Expected block {:?}, got: {message:?}", $expected)
            }
        };
    }

    async fn trigger_fb_and_assert_next_block(
        stream: &mut RobustSubscriptionStream<alloy::network::Ethereum>,
        fallback_provider: RootProvider,
        expected_block: u64,
    ) -> anyhow::Result<()> {
        let task = tokio::spawn(async move {
            sleep(SHORT_TIMEOUT + BUFFER_TIME).await;
            fallback_provider.anvil_mine(Some(1), None).await.unwrap();
        });
        assert_next_block!(*stream, expected_block);
        task.await?;
        Ok(())
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

        assert!(matches!(result, Err(Error::Timeout)));
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
    async fn ws_fails_http_fallback_returns_primary_error() -> anyhow::Result<()> {
        // Setup: Create WS primary and HTTP fallback
        let anvil_1 = Anvil::new().try_spawn()?;
        let ws_provider =
            ProviderBuilder::new().connect(anvil_1.ws_endpoint_url().as_str()).await?;

        let anvil_2 = Anvil::new().try_spawn()?;
        let http_provider = ProviderBuilder::new().connect_http(anvil_2.endpoint_url());

        let robust = RobustProviderBuilder::fragile(ws_provider.clone())
            .fallback(http_provider.clone())
            .subscription_timeout(Duration::from_secs(1))
            .build()
            .await?;

        // Test: Verify subscription works on primary
        let subscription = robust.subscribe_blocks().await?;
        let mut stream = subscription.into_stream();

        ws_provider.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 1);

        ws_provider.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 2);

        // Verify: HTTP fallback can't provide subscription, so we get an error
        let task = tokio::spawn(async move {
            sleep(SHORT_TIMEOUT + BUFFER_TIME).await;
            http_provider.anvil_mine(Some(1), None).await.unwrap();
        });
        let err = stream.next().await.unwrap().unwrap_err();
        task.await?;
        assert_backend_gone_or_timeout(err);

        let next = stream.next().await;
        assert!(next.is_none(), "Expected stream to be finished, got: {next:?}");

        Ok(())
    }

    #[tokio::test]
    async fn robust_subscription_stream_with_failover() -> anyhow::Result<()> {
        let (_anvil_1, primary) = spawn_ws_anvil().await?;
        let (_anvil_2, fallback) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(primary.clone())
            .fallback(fallback.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .build()
            .await?;

        let subscription = robust.subscribe_blocks().await?;
        let mut stream = subscription.into_stream();

        // Test: Primary works initially
        primary.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 1);

        primary.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 2);

        // After timeout, should failover to fallback provider
        trigger_fb_and_assert_next_block(&mut stream, fallback.clone(), 1).await?;

        fallback.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 2);

        Ok(())
    }

    #[tokio::test]
    async fn subscription_reconnects_to_primary() -> anyhow::Result<()> {
        let (_anvil_1, primary) = spawn_ws_anvil().await?;
        let (_anvil_2, fallback) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(primary.clone())
            .fallback(fallback.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .reconnect_interval(RECONNECT_INTERVAL)
            .build()
            .await?;

        let subscription = robust.subscribe_blocks().await?;
        let mut stream = subscription.into_stream();

        // Test: Start on primary
        primary.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 1);

        // Trigger failover to fallback
        trigger_fb_and_assert_next_block(&mut stream, fallback.clone(), 1).await?;

        fallback.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 2);

        // Wait for reconnect interval to trigger primary reconnection
        sleep(RECONNECT_INTERVAL + BUFFER_TIME).await;

        // Mine on primary after reconnection window
        let reconnect_task =
            tokio::spawn(mine_after_delay(primary.clone(), 1, Duration::from_millis(50)));

        // Verify: Successfully reconnected to primary (block 2 on primary chain)
        assert_next_block!(stream, 2);
        reconnect_task.await?;

        assert_stream_finished!(stream);

        Ok(())
    }

    #[tokio::test]
    async fn subscription_cycles_through_multiple_fallbacks() -> anyhow::Result<()> {
        // Setup: Three providers, will cycle through all
        let (_anvil_1, provider_1) = spawn_ws_anvil().await?;
        let (_anvil_2, provider_2) = spawn_ws_anvil().await?;
        let (_anvil_3, provider_3) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(provider_1.clone())
            .fallback(provider_2.clone())
            .fallback(provider_3.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .build()
            .await?;

        let subscription = robust.subscribe_blocks().await?;
        let mut stream = subscription.into_stream();

        // Test: Start on primary
        provider_1.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 1);

        // Failover to second provider
        trigger_fb_and_assert_next_block(&mut stream, provider_2.clone(), 1).await?;

        // Failover to third provider
        trigger_fb_and_assert_next_block(&mut stream, provider_3.clone(), 1).await?;

        Ok(())
    }

    #[tokio::test]
    async fn subscription_fails_with_no_fallbacks() -> anyhow::Result<()> {
        // Setup: Single provider with no fallbacks
        let (_anvil, provider) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(provider.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .build()
            .await?;

        let subscription = robust.subscribe_blocks().await?;
        let mut stream = subscription.into_stream();

        // Test: Provider works initially
        provider.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 1);

        // Verify: No fallback available, should error
        let task = tokio::spawn(async move {
            sleep(SHORT_TIMEOUT + BUFFER_TIME).await;
            provider.anvil_mine(Some(1), None).await.unwrap();
        });
        let err = stream.next().await.unwrap().unwrap_err();
        task.await?;

        assert_backend_gone_or_timeout(err);

        Ok(())
    }
}
