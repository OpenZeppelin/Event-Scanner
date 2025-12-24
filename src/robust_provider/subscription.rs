use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, ready},
    time::{Duration, Instant},
};

use alloy::{
    network::Network,
    providers::{Provider, RootProvider},
    pubsub::Subscription,
    transports::{RpcError, TransportErrorKind},
};
use thiserror::Error;
use tokio::{sync::broadcast::error::RecvError, time::timeout};
use tokio_stream::Stream;
use tokio_util::sync::ReusableBoxFuture;

use crate::robust_provider::{RobustProvider, provider::CoreError};

/// Errors that can occur when using [`RobustSubscription`].
#[derive(Error, Debug, Clone)]
pub enum Error {
    #[error("Operation timed out")]
    Timeout,
    #[error("RPC call failed after exhausting all retry attempts: {0}")]
    RpcError(Arc<RpcError<TransportErrorKind>>),
    #[error("Subscription closed")]
    Closed,
    #[error("Subscription lagged behind by: {0}")]
    Lagged(u64),
}

impl From<CoreError> for Error {
    fn from(err: CoreError) -> Self {
        match err {
            CoreError::Timeout => Error::Timeout,
            CoreError::RpcError(e) => Error::RpcError(Arc::new(e)),
        }
    }
}

impl From<RecvError> for Error {
    fn from(err: RecvError) -> Self {
        match err {
            RecvError::Closed => Error::Closed,
            RecvError::Lagged(count) => Error::Lagged(count),
        }
    }
}

impl From<tokio::time::error::Elapsed> for Error {
    fn from(_: tokio::time::error::Elapsed) -> Self {
        Error::Timeout
    }
}

/// Default time interval between primary provider reconnection attempts
pub const DEFAULT_RECONNECT_INTERVAL: Duration = Duration::from_secs(30);

/// A robust subscription wrapper that automatically handles provider failover
/// and periodic reconnection attempts to the primary provider.
#[derive(Debug)]
pub struct RobustSubscription<N: Network> {
    subscription: Subscription<N::HeaderResponse>,
    robust_provider: RobustProvider<N>,
    last_reconnect_attempt: Option<Instant>,
    current_fallback_index: Option<usize>,
}

impl<N: Network> RobustSubscription<N> {
    /// Create a new [`RobustSubscription`]
    pub(crate) fn new(
        subscription: Subscription<N::HeaderResponse>,
        robust_provider: RobustProvider<N>,
    ) -> Self {
        Self {
            subscription,
            robust_provider,
            last_reconnect_attempt: None,
            current_fallback_index: None,
        }
    }

    /// Receive the next item from the subscription with automatic failover.
    ///
    /// This method will:
    /// * Attempt to receive from the current subscription
    /// * Handle errors by switching to fallback providers
    /// * Periodically attempt to reconnect to the primary provider
    /// * Will switch to fallback providers if subscription timeout is exhausted
    ///
    /// # Primary Provider Reconnection
    ///
    /// The primary provider is retried in two scenarios:
    /// 1. **Periodic reconnection**: Every `reconnect_interval` (default: 30 seconds) while on a
    ///    fallback provider and successfully receiving blocks. Note: The actual reconnection
    ///    attempt occurs when a new block is received, so if blocks arrive slower than the
    ///    reconnect interval, reconnection will be delayed until the next block.
    /// 2. **Fallback failure**: Immediately when a fallback provider fails, before attempting the
    ///    next fallback provider
    ///
    /// # Errors
    ///
    /// * Propagates any underlying subscription errors.
    /// * If all providers have been exhausted and failed, returns the last attempt's error.
    pub async fn recv(&mut self) -> Result<N::HeaderResponse, Error> {
        let subscription_timeout = self.robust_provider.subscription_timeout;

        loop {
            match timeout(subscription_timeout, self.subscription.recv()).await {
                Ok(Ok(header)) => {
                    if self.is_on_fallback() {
                        self.try_reconnect_to_primary(false).await;
                    }
                    return Ok(header);
                }
                Ok(Err(recv_error)) => return Err(recv_error.into()),
                Err(_elapsed) => {
                    warn!(
                        timeout_secs = subscription_timeout.as_secs(),
                        "Subscription timeout - no block received, switching provider"
                    );
                    self.switch_to_fallback(CoreError::Timeout).await?;
                }
            }
        }
    }

    /// Try to reconnect to the primary provider if enough time has elapsed.
    /// Returns true if reconnection was successful, false if it's not time yet or if it failed.
    async fn try_reconnect_to_primary(&mut self, force: bool) -> bool {
        // Check if we should attempt reconnection
        let should_reconnect = force ||
            match self.last_reconnect_attempt {
                None => false,
                Some(last_attempt) => {
                    last_attempt.elapsed() >= self.robust_provider.reconnect_interval
                }
            };

        if !should_reconnect {
            return false;
        }

        let operation =
            move |provider: RootProvider<N>| async move { provider.subscribe_blocks().await };

        let primary = self.robust_provider.primary();
        let subscription =
            self.robust_provider.try_provider_with_timeout(primary, &operation).await;

        if let Ok(sub) = subscription {
            info!("Reconnected to primary provider");
            self.subscription = sub;
            self.current_fallback_index = None;
            self.last_reconnect_attempt = None;
            true
        } else {
            self.last_reconnect_attempt = Some(Instant::now());
            false
        }
    }

    async fn switch_to_fallback(&mut self, last_error: CoreError) -> Result<(), Error> {
        // If we're on a fallback, try primary first before moving to next fallback
        if self.is_on_fallback() && self.try_reconnect_to_primary(true).await {
            return Ok(());
        }

        if self.last_reconnect_attempt.is_none() {
            self.last_reconnect_attempt = Some(Instant::now());
        }

        let operation =
            move |provider: RootProvider<N>| async move { provider.subscribe_blocks().await };

        // Start searching from the next provider after the current one
        let start_index = self.current_fallback_index.map_or(0, |idx| idx + 1);

        let (sub, fallback_idx) = self
            .robust_provider
            .try_fallback_providers_from(&operation, true, last_error, start_index)
            .await?;

        info!(fallback_index = fallback_idx, "Subscription switched to fallback provider");
        self.subscription = sub;
        self.current_fallback_index = Some(fallback_idx);
        Ok(())
    }

    /// Returns true if currently using a fallback provider
    fn is_on_fallback(&self) -> bool {
        self.current_fallback_index.is_some()
    }

    /// Check if the subscription channel is empty (no pending messages)
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.subscription.is_empty()
    }

    /// Convert the subscription into a stream.
    #[must_use]
    pub fn into_stream(self) -> RobustSubscriptionStream<N> {
        RobustSubscriptionStream::from(self)
    }
}

type SubscriptionResult<N> = (Result<<N as Network>::HeaderResponse, Error>, RobustSubscription<N>);

pub struct RobustSubscriptionStream<N: Network> {
    inner: ReusableBoxFuture<'static, SubscriptionResult<N>>,
}

async fn make_future<N: Network>(mut rx: RobustSubscription<N>) -> SubscriptionResult<N> {
    let result = rx.recv().await;
    (result, rx)
}

impl<N: 'static + Clone + Send + Network> RobustSubscriptionStream<N> {
    /// Create a new `RobustSubscriptionStream`.
    #[must_use]
    pub fn new(rx: RobustSubscription<N>) -> Self {
        Self { inner: ReusableBoxFuture::new(make_future(rx)) }
    }
}

impl<N: 'static + Clone + Send + Network> Stream for RobustSubscriptionStream<N> {
    type Item = Result<N::HeaderResponse, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let (result, rx) = ready!(self.inner.poll(cx));
        self.inner.set(make_future(rx));
        match result {
            Ok(item) => Poll::Ready(Some(Ok(item))),
            Err(Error::Closed) => Poll::Ready(None),
            Err(e) => Poll::Ready(Some(Err(e))),
        }
    }
}

impl<N: 'static + Clone + Send + Network> From<RobustSubscription<N>>
    for RobustSubscriptionStream<N>
{
    fn from(recv: RobustSubscription<N>) -> Self {
        Self::new(recv)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::robust_provider::{DEFAULT_SUBSCRIPTION_BUFFER_CAPACITY, RobustProviderBuilder};
    use alloy::{
        network::Ethereum,
        providers::{Provider, ProviderBuilder, RootProvider, ext::AnvilApi},
    };
    use alloy_node_bindings::{Anvil, AnvilInstance};
    use tokio::time::sleep;
    use tokio_stream::StreamExt;

    const SHORT_TIMEOUT: Duration = Duration::from_millis(300);
    const RECONNECT_INTERVAL: Duration = Duration::from_millis(500);
    const BUFFER_TIME: Duration = Duration::from_millis(100);

    async fn spawn_ws_anvil() -> anyhow::Result<(AnvilInstance, RootProvider)> {
        let anvil = Anvil::new().try_spawn()?;
        let provider = ProviderBuilder::new().connect(anvil.ws_endpoint_url().as_str()).await?;
        Ok((anvil, provider.root().to_owned()))
    }

    macro_rules! assert_next_block {
        ($stream: expr, $expected: expr) => {
            assert_next_block!($stream, $expected, timeout = 5)
        };
        ($stream: expr, $expected: expr, timeout = $secs: expr) => {
            let block = tokio::time::timeout(
                std::time::Duration::from_secs($secs),
                tokio_stream::StreamExt::next(&mut $stream),
            )
            .await
            .expect("timed out")
            .unwrap();
            let block = block.unwrap();
            assert_eq!(block.number, $expected);
        };
    }

    /// Waits for current provider to timeout, then mines on `next_provider` to trigger failover.
    async fn trigger_failover_with_delay(
        stream: &mut RobustSubscriptionStream<Ethereum>,
        next_provider: RootProvider,
        expected_block: u64,
        extra_delay: Duration,
    ) -> anyhow::Result<()> {
        let task = tokio::spawn(async move {
            sleep(SHORT_TIMEOUT + extra_delay + BUFFER_TIME).await;
            next_provider.anvil_mine(Some(1), None).await.unwrap();
        });
        assert_next_block!(*stream, expected_block);
        task.await?;
        Ok(())
    }

    async fn trigger_failover(
        stream: &mut RobustSubscriptionStream<alloy::network::Ethereum>,
        next_provider: RootProvider,
        expected_block: u64,
    ) -> anyhow::Result<()> {
        trigger_failover_with_delay(stream, next_provider, expected_block, Duration::ZERO).await
    }

    // ----------------------------------------------------------------------------
    // Basic Subscription Tests
    // ----------------------------------------------------------------------------

    #[tokio::test]
    async fn test_successful_subscription_on_primary() -> anyhow::Result<()> {
        let (_anvil, provider) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(provider.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .build()
            .await?;

        let subscription = robust.subscribe_blocks().await?;
        // Subscription is created successfully - is_empty() returns true initially (no pending
        // messages)
        assert!(subscription.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn test_multiple_consecutive_recv_calls() -> anyhow::Result<()> {
        let (_anvil, provider) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(provider.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .build()
            .await?;

        let mut subscription = robust.subscribe_blocks().await?;

        for i in 1..=5 {
            provider.anvil_mine(Some(1), None).await?;
            let block = subscription.recv().await?;
            assert_eq!(block.number, i);
        }

        Ok(())
    }

    // ----------------------------------------------------------------------------
    // Stream Tests
    // ----------------------------------------------------------------------------

    #[tokio::test]
    async fn test_convert_subscription_to_stream() -> anyhow::Result<()> {
        let (_anvil, provider) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(provider.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .build()
            .await?;

        let subscription = robust.subscribe_blocks().await?;

        // Convert to stream
        let mut stream = subscription.into_stream();

        // Use the stream
        provider.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 1);

        Ok(())
    }

    #[tokio::test]
    async fn test_stream_consuming_multiple_blocks() -> anyhow::Result<()> {
        let (_anvil, provider) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(provider.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .build()
            .await?;

        let subscription = robust.subscribe_blocks().await?;
        let mut stream = subscription.into_stream();

        for i in 1..=5 {
            provider.anvil_mine(Some(1), None).await?;
            assert_next_block!(stream, i);
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_stream_consumes_multiple_blocks_in_sequence() -> anyhow::Result<()> {
        let (_anvil, provider) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(provider.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .build()
            .await?;

        let subscription = robust.subscribe_blocks().await?;
        let mut stream = subscription.into_stream();

        provider.anvil_mine(Some(5), None).await?;
        assert_next_block!(stream, 1);
        assert_next_block!(stream, 2);
        assert_next_block!(stream, 3);
        assert_next_block!(stream, 4);
        assert_next_block!(stream, 5);

        Ok(())
    }

    #[tokio::test]
    async fn test_stream_creation() -> anyhow::Result<()> {
        let (_anvil, provider) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(provider.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .build()
            .await?;

        let subscription = robust.subscribe_blocks().await?;
        let mut stream = subscription.into_stream();

        // Stream should work normally
        provider.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 1);

        Ok(())
    }

    #[tokio::test]
    async fn test_stream_continues_streaming_errors() -> anyhow::Result<()> {
        let (_anvil, provider) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(provider.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .build()
            .await?;

        let subscription = robust.subscribe_blocks().await?;
        let mut stream = subscription.into_stream();

        // Get one block
        provider.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 1);

        // Trigger timeout error - the stream will continue to stream errors
        assert!(matches!(stream.next().await.unwrap(), Err(Error::Timeout)));

        // Without fallbacks, subsequent calls will continue to return errors
        // (not None, since only Error::Closed terminates the stream)
        assert!(matches!(stream.next().await.unwrap(), Err(Error::Timeout)));

        Ok(())
    }

    // ----------------------------------------------------------------------------
    // Basic Failover Tests
    // ----------------------------------------------------------------------------

    #[tokio::test]
    async fn robust_subscription_stream_with_failover() -> anyhow::Result<()> {
        let (_anvil_1, primary) = spawn_ws_anvil().await?;
        let (_anvil_2, fallback) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(primary.clone())
            .fallback(fallback.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .build()
            .await?;

        let mut subscription = robust.subscribe_blocks().await?;

        // Initial state: on primary
        assert!(subscription.current_fallback_index.is_none());
        assert!(subscription.last_reconnect_attempt.is_none());

        // Test: Primary works initially
        primary.anvil_mine(Some(1), None).await?;
        assert_eq!(subscription.recv().await?.number, 1);

        primary.anvil_mine(Some(1), None).await?;
        assert_eq!(subscription.recv().await?.number, 2);

        // After timeout, should failover to fallback provider
        let fb = fallback.clone();
        tokio::spawn(async move {
            sleep(SHORT_TIMEOUT + BUFFER_TIME).await;
            fb.anvil_mine(Some(1), None).await.unwrap();
        });
        assert_eq!(subscription.recv().await?.number, 1);

        // After failover: on fallback[0]
        assert_eq!(subscription.current_fallback_index, Some(0));
        assert!(subscription.last_reconnect_attempt.is_some());

        // PP is not used after failover
        primary.anvil_mine(Some(1), None).await?;
        fallback.anvil_mine(Some(1), None).await?;

        // From fallback, not primary's block 3
        assert_eq!(subscription.recv().await?.number, 2);

        Ok(())
    }

    #[tokio::test]
    async fn subscription_fails_with_no_fallbacks() -> anyhow::Result<()> {
        let (_anvil, provider) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(provider.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .build()
            .await?;

        let subscription = robust.subscribe_blocks().await?;
        let mut stream = subscription.into_stream();

        provider.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 1);

        // No fallback available - should error after timeout
        assert!(matches!(stream.next().await.unwrap(), Err(Error::Timeout)));

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
        assert!(matches!(stream.next().await.unwrap(), Err(Error::Timeout)));

        Ok(())
    }

    // ----------------------------------------------------------------------------
    // Fallback Cycling Tests
    // ----------------------------------------------------------------------------

    #[tokio::test]
    async fn test_single_fallback_provider() -> anyhow::Result<()> {
        let (anvil_pp, primary) = spawn_ws_anvil().await?;
        let (_anvil_2, fallback) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(primary.clone())
            .fallback(fallback.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .call_timeout(SHORT_TIMEOUT)
            .build()
            .await?;

        let subscription = robust.subscribe_blocks().await?;
        let mut stream = subscription.into_stream();

        // Start on primary
        primary.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 1);

        // Kill primary so reconnect attempts fail
        drop(anvil_pp);

        // PP -> FB
        trigger_failover(&mut stream, fallback.clone(), 1).await?;

        // FB -> try PP (fails) -> no more fallbacks -> error
        assert!(matches!(stream.next().await.unwrap(), Err(Error::Timeout)));

        Ok(())
    }

    #[tokio::test]
    async fn subscription_cycles_through_multiple_fallbacks() -> anyhow::Result<()> {
        let (anvil_pp, primary) = spawn_ws_anvil().await?;
        let (_anvil_1, fb_1) = spawn_ws_anvil().await?;
        let (_anvil_2, fb_2) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(primary.clone())
            .fallback(fb_1.clone())
            .fallback(fb_2.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .call_timeout(SHORT_TIMEOUT)
            .build()
            .await?;

        let subscription = robust.subscribe_blocks().await?;
        let mut stream = subscription.into_stream();

        // Start on primary
        primary.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 1);

        // Kill primary - all future PP reconnection attempts will fail
        drop(anvil_pp);

        // PP times out -> FP1
        trigger_failover(&mut stream, fb_1.clone(), 1).await?;

        // FP1 times out -> tries PP (fails, takes call_timeout) -> FP2
        trigger_failover_with_delay(&mut stream, fb_2.clone(), 1, SHORT_TIMEOUT).await?;

        fb_2.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 2);

        // FP2 times out -> tries PP (fails) -> no more fallbacks -> error
        assert!(matches!(stream.next().await.unwrap(), Err(Error::Timeout)));

        Ok(())
    }

    #[tokio::test]
    async fn test_many_fallback_providers() -> anyhow::Result<()> {
        let (anvil_pp, primary) = spawn_ws_anvil().await?;
        let (_anvil_1, fb_1) = spawn_ws_anvil().await?;
        let (_anvil_2, fb_2) = spawn_ws_anvil().await?;
        let (_anvil_3, fb_3) = spawn_ws_anvil().await?;
        let (_anvil_4, fb_4) = spawn_ws_anvil().await?;
        let (_anvil_5, fb_5) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(primary.clone())
            .fallback(fb_1.clone())
            .fallback(fb_2.clone())
            .fallback(fb_3.clone())
            .fallback(fb_4.clone())
            .fallback(fb_5.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .call_timeout(SHORT_TIMEOUT)
            .build()
            .await?;

        let subscription = robust.subscribe_blocks().await?;
        let mut stream = subscription.into_stream();

        // Start on primary
        primary.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 1);

        // Kill primary
        drop(anvil_pp);

        // Cycle through all fallbacks
        trigger_failover(&mut stream, fb_1.clone(), 1).await?;
        trigger_failover_with_delay(&mut stream, fb_2.clone(), 1, SHORT_TIMEOUT).await?;
        trigger_failover_with_delay(&mut stream, fb_3.clone(), 1, SHORT_TIMEOUT).await?;
        trigger_failover_with_delay(&mut stream, fb_4.clone(), 1, SHORT_TIMEOUT).await?;
        trigger_failover_with_delay(&mut stream, fb_5.clone(), 1, SHORT_TIMEOUT).await?;

        assert!(matches!(stream.next().await.unwrap(), Err(Error::Timeout)));

        Ok(())
    }

    // ----------------------------------------------------------------------------
    // Reconnection Tests
    // ----------------------------------------------------------------------------

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

        // Start on primary
        primary.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 1);

        // PP times out -> FP1
        trigger_failover(&mut stream, fallback.clone(), 1).await?;

        fallback.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 2);

        // FP1 times out -> PP (reconnect succeeds)
        trigger_failover(&mut stream, primary.clone(), 2).await?;

        // PP times out -> FP1 (fallback index was reset)
        trigger_failover(&mut stream, fallback.clone(), 3).await?;

        fallback.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 4);

        Ok(())
    }

    #[tokio::test]
    async fn subscription_periodically_reconnects_to_primary_while_on_fallback()
    -> anyhow::Result<()> {
        // Use a longer reconnect interval to make timing more predictable
        let reconnect_interval = Duration::from_millis(800);

        let (_anvil_1, primary) = spawn_ws_anvil().await?;
        let (_anvil_2, fallback) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(primary.clone())
            .fallback(fallback.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .reconnect_interval(reconnect_interval)
            .build()
            .await?;

        let subscription = robust.subscribe_blocks().await?;
        let mut stream = subscription.into_stream();

        // Start on primary
        primary.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 1);

        // PP times out -> FP (this sets last_reconnect_attempt)
        trigger_failover(&mut stream, fallback.clone(), 1).await?;
        let failover_time = Instant::now();

        // Now on fallback - mine blocks before reconnect_interval elapses
        // These should stay on fallback (no reconnect attempt)
        fallback.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 2);

        fallback.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 3);

        // Ensure reconnect_interval has fully elapsed since failover
        let elapsed = failover_time.elapsed();
        if elapsed < reconnect_interval + BUFFER_TIME {
            sleep(reconnect_interval + BUFFER_TIME - elapsed).await;
        }

        // Mine on fallback - receiving this block triggers try_reconnect_to_primary
        fallback.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 4);

        // Now we should be back on primary
        primary.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 2);

        primary.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 3);

        Ok(())
    }

    #[tokio::test]
    async fn test_reconnection_skipped_before_interval_elapsed() -> anyhow::Result<()> {
        let (_anvil_1, primary) = spawn_ws_anvil().await?;
        let (_anvil_2, fallback) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(primary.clone())
            .fallback(fallback.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .reconnect_interval(Duration::from_secs(10)) // Long interval
            .build()
            .await?;

        let subscription = robust.subscribe_blocks().await?;
        let mut stream = subscription.into_stream();

        // Start on primary
        primary.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 1);
        primary.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 2);

        // Failover to fallback
        trigger_failover(&mut stream, fallback.clone(), 1).await?;

        // Immediately try another recv - should stay on fallback (no reconnect attempt)
        fallback.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 2);

        Ok(())
    }

    #[tokio::test]
    async fn test_successful_reconnection_resets_state() -> anyhow::Result<()> {
        let (_anvil_1, primary) = spawn_ws_anvil().await?;
        let (_anvil_2, fb_1) = spawn_ws_anvil().await?;
        let (_anvil_3, fb_2) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(primary.clone())
            .fallback(fb_1.clone())
            .fallback(fb_2.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .reconnect_interval(RECONNECT_INTERVAL)
            .build()
            .await?;

        let subscription = robust.subscribe_blocks().await?;
        let mut stream = subscription.into_stream();

        // Start on primary
        primary.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 1);

        // Failover to fallback
        trigger_failover(&mut stream, fb_1.clone(), 1).await?;

        fb_1.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 2);

        // Wait for reconnect interval, then timeout - reconnect to primary
        sleep(RECONNECT_INTERVAL).await;
        trigger_failover(&mut stream, primary.clone(), 2).await?;

        // After reconnection, next failover should go to fallback[0] again (not fallback[1])
        trigger_failover(&mut stream, fb_1.clone(), 3).await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_multiple_failed_reconnection_attempts() -> anyhow::Result<()> {
        let (anvil_pp, primary) = spawn_ws_anvil().await?;
        let (_anvil_1, fb_1) = spawn_ws_anvil().await?;
        let (_anvil_2, fb_2) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(primary.clone())
            .fallback(fb_1.clone())
            .fallback(fb_2.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .reconnect_interval(RECONNECT_INTERVAL)
            .call_timeout(SHORT_TIMEOUT)
            .build()
            .await?;

        let subscription = robust.subscribe_blocks().await?;
        let mut stream = subscription.into_stream();

        // Start on primary
        primary.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 1);

        // Kill primary
        drop(anvil_pp);

        // Failover to fb_1 (primary is dead)
        trigger_failover(&mut stream, fb_1.clone(), 1).await?;

        // Stay on fb_1 for a bit
        fb_1.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 2);

        // Wait for reconnect interval, then timeout - try primary (fails), go to fb_2
        sleep(RECONNECT_INTERVAL).await;
        trigger_failover_with_delay(&mut stream, fb_2.clone(), 1, SHORT_TIMEOUT).await?;

        // fb_2 continues to work
        fb_2.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 2);

        Ok(())
    }

    #[tokio::test]
    async fn test_primary_reconnect_attempt_before_next_fallback() -> anyhow::Result<()> {
        let (_anvil_1, primary) = spawn_ws_anvil().await?;
        let (_anvil_2, fb_1) = spawn_ws_anvil().await?;
        let (_anvil_3, fb_2) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(primary.clone())
            .fallback(fb_1.clone())
            .fallback(fb_2.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .reconnect_interval(RECONNECT_INTERVAL)
            .build()
            .await?;

        let subscription = robust.subscribe_blocks().await?;
        let mut stream = subscription.into_stream();

        // Start on primary
        primary.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 1);

        // PP -> FB1
        trigger_failover(&mut stream, fb_1.clone(), 1).await?;

        // FB1 -> PP (reconnect succeeds, not FB2)
        trigger_failover(&mut stream, primary.clone(), 2).await?;

        Ok(())
    }

    // ----------------------------------------------------------------------------
    // Error Propagation Tests
    // ----------------------------------------------------------------------------

    #[tokio::test]
    async fn test_backend_gone_error_propagation() -> anyhow::Result<()> {
        let (anvil, provider) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(provider.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .build()
            .await?;

        let subscription = robust.subscribe_blocks().await?;
        let mut stream = subscription.into_stream();

        // Get one block
        provider.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 1);

        // Kill the provider
        drop(anvil);

        // Should get BackendGone or Timeout error
        assert!(matches!(stream.next().await.unwrap(), Err(Error::Timeout)));

        Ok(())
    }

    #[tokio::test]
    async fn test_immediate_consecutive_failures() -> anyhow::Result<()> {
        let (anvil, provider) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(provider.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .build()
            .await?;

        let subscription = robust.subscribe_blocks().await?;
        let mut stream = subscription.into_stream();

        // Get one block
        provider.anvil_mine(Some(1), None).await?;
        assert_next_block!(stream, 1);

        // Kill provider immediately
        drop(anvil);

        // First failure
        assert!(matches!(stream.next().await.unwrap(), Err(Error::Timeout)));

        Ok(())
    }

    #[tokio::test]
    async fn test_subscription_lagged_error() -> anyhow::Result<()> {
        let (_anvil, provider) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(provider.clone())
            .subscription_timeout(Duration::from_secs(5))
            .build()
            .await?;

        let mut subscription = robust.subscribe_blocks().await?;

        // Mine more blocks than channel can hold without consuming
        provider.anvil_mine(Some(DEFAULT_SUBSCRIPTION_BUFFER_CAPACITY as u64 + 1), None).await?;

        // Allow time for block notifications to propagate through WebSocket
        // and fill the subscription channel
        sleep(BUFFER_TIME).await;

        // First recv should return Lagged error (skipped some blocks)
        let result = subscription.recv().await;
        assert!(matches!(result, Err(Error::Lagged(_))));

        Ok(())
    }
}
