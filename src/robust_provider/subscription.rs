use std::{
    pin::Pin,
    task::{Context, Poll, ready},
    time::{Duration, Instant},
};

use alloy::{
    network::Network,
    providers::{Provider, RootProvider},
    pubsub::Subscription,
    transports::{RpcError, TransportErrorKind},
};
use tokio::{sync::broadcast::error::RecvError, time::timeout};
use tokio_stream::Stream;
use tokio_util::sync::ReusableBoxFuture;
use tracing::{error, info, warn};

use crate::robust_provider::{Error, RobustProvider};

/// Default time interval between primary provider reconnection attempts
pub const DEFAULT_RECONNECT_INTERVAL: Duration = Duration::from_secs(30);

/// Maximum number of consecutive lags before switching providers
const MAX_LAG_COUNT: usize = 3;

/// A robust subscription wrapper that automatically handles provider failover
/// and periodic reconnection attempts to the primary provider.
#[derive(Debug)]
pub struct RobustSubscription<N: Network> {
    subscription: Option<Subscription<N::HeaderResponse>>,
    robust_provider: RobustProvider<N>,
    last_reconnect_attempt: Option<Instant>,
    consecutive_lags: usize,
    current_fallback_index: Option<usize>,
}

impl<N: Network> RobustSubscription<N> {
    /// Create a new [`RobustSubscription`]
    pub(crate) fn new(
        subscription: Subscription<N::HeaderResponse>,
        robust_provider: RobustProvider<N>,
    ) -> Self {
        Self {
            subscription: Some(subscription),
            robust_provider,
            last_reconnect_attempt: None,
            consecutive_lags: 0,
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
    /// # Errors
    ///
    /// Returns an error if all providers have been exhausted and failed.
    pub async fn recv(&mut self) -> Result<N::HeaderResponse, Error> {
        let subscription_timeout = self.robust_provider.subscription_timeout;
        while let Some(subscription) = &mut self.subscription {
            let recv_result = timeout(subscription_timeout, subscription.recv()).await;
            match recv_result {
                Ok(recv_result) => match recv_result {
                    Ok(header) => {
                        if self.is_on_fallback() {
                            self.try_reconnect_to_primary(false).await;
                        }
                        self.consecutive_lags = 0;
                        return Ok(header);
                    }
                    Err(recv_error) => {
                        self.process_recv_error(recv_error).await?;
                    }
                },
                Err(elapsed_err) => {
                    error!(
                        timeout_secs = subscription_timeout.as_secs(),
                        "Subscription timeout - no block received, switching provider"
                    );

                    self.switch_to_fallback(elapsed_err.into()).await?;
                }
            }
        }
        // No subscription available
        Err(RpcError::Transport(TransportErrorKind::BackendGone).into())
    }

    /// Process subscription receive errors and handle failover
    async fn process_recv_error(&mut self, recv_error: RecvError) -> Result<(), Error> {
        match recv_error {
            RecvError::Closed => {
                error!("Subscription channel closed, switching provider");
                let error = RpcError::Transport(TransportErrorKind::BackendGone).into();
                self.switch_to_fallback(error).await?;
            }
            RecvError::Lagged(skipped) => {
                self.consecutive_lags += 1;
                warn!(
                    skipped = skipped,
                    consecutive_lags = self.consecutive_lags,
                    "Subscription lagged"
                );

                if self.consecutive_lags >= MAX_LAG_COUNT {
                    error!("Too many consecutive lags, switching provider");
                    let error = RpcError::Transport(TransportErrorKind::BackendGone).into();
                    self.switch_to_fallback(error).await?;
                }
            }
        }
        Ok(())
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

        info!("Attempting to reconnect to primary provider");

        let operation =
            move |provider: RootProvider<N>| async move { provider.subscribe_blocks().await };

        let primary = self.robust_provider.primary();
        let subscription =
            self.robust_provider.try_provider_with_timeout(primary, &operation).await;

        match subscription {
            Ok(sub) => {
                info!("Successfully reconnected to primary provider");
                self.subscription = Some(sub);
                self.current_fallback_index = None;
                self.last_reconnect_attempt = None;
                true
            }
            Err(e) => {
                self.last_reconnect_attempt = Some(Instant::now());
                warn!(error = %e, "Failed to reconnect to primary provider");
                false
            }
        }
    }

    async fn switch_to_fallback(&mut self, last_error: Error) -> Result<(), Error> {
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

        let subscription = self
            .robust_provider
            .try_fallback_providers_from(&operation, true, last_error, start_index)
            .await;

        match subscription {
            Ok((sub, fallback_idx)) => {
                self.subscription = Some(sub);
                self.current_fallback_index = Some(fallback_idx);
                Ok(())
            }
            Err(e) => {
                error!(error = %e, "eth_subscribe failed - no fallbacks available");
                Err(e)
            }
        }
    }

    /// Returns true if currently using a fallback provider
    fn is_on_fallback(&self) -> bool {
        self.current_fallback_index.is_some()
    }

    /// Check if the subscription channel is empty (no pending messages)
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match &self.subscription {
            Some(sub) => sub.is_empty(),
            None => true,
        }
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
    finished: bool,
}

async fn make_future<N: Network>(mut rx: RobustSubscription<N>) -> SubscriptionResult<N> {
    let result = rx.recv().await;
    (result, rx)
}

impl<N: 'static + Clone + Send + Network> RobustSubscriptionStream<N> {
    /// Create a new `RobustSubscriptionStream`.
    #[must_use]
    pub fn new(rx: RobustSubscription<N>) -> Self {
        Self { inner: ReusableBoxFuture::new(make_future(rx)), finished: false }
    }

    /// Returns true if the stream has reached a terminal state.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.finished
    }
}

impl<N: 'static + Clone + Send + Network> Stream for RobustSubscriptionStream<N> {
    type Item = Result<N::HeaderResponse, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.finished {
            return Poll::Ready(None);
        }

        let (result, rx) = ready!(self.inner.poll(cx));

        match result {
            Ok(item) => {
                self.inner.set(make_future(rx));
                Poll::Ready(Some(Ok(item)))
            }
            Err(e) => {
                self.finished = true;
                Poll::Ready(Some(Err(e)))
            }
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

    use alloy::{
        providers::{Provider, ProviderBuilder, RootProvider, ext::AnvilApi},
        transports::{RpcError, TransportErrorKind},
    };
    use alloy_node_bindings::{Anvil, AnvilInstance};
    use tokio::time::sleep;
    use tokio_stream::StreamExt;

    use crate::robust_provider::{Error, RobustProviderBuilder};

    const SHORT_TIMEOUT: Duration = Duration::from_millis(300);
    const RECONNECT_INTERVAL: Duration = Duration::from_millis(500);
    const BUFFER_TIME: Duration = Duration::from_millis(100);

    async fn spawn_ws_anvil() -> anyhow::Result<(AnvilInstance, RootProvider)> {
        let anvil = Anvil::new().try_spawn()?;
        let provider = ProviderBuilder::new().connect(anvil.ws_endpoint_url().as_str()).await?;
        Ok((anvil, provider.root().to_owned()))
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

    /// Waits for current provider to timeout, then mines on `next_provider` to trigger failover.
    async fn trigger_failover_with_delay(
        stream: &mut RobustSubscriptionStream<alloy::network::Ethereum>,
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

    /// Waits for timeout and asserts a backend gone or timeout error.
    async fn assert_timeout_error(
        stream: &mut RobustSubscriptionStream<alloy::network::Ethereum>,
        extra_delay: Duration,
    ) {
        sleep(SHORT_TIMEOUT + extra_delay + BUFFER_TIME).await;
        let err = stream.next().await.unwrap().unwrap_err();
        assert_backend_gone_or_timeout(err);
        let next = stream.next().await;
        assert!(next.is_none(), "Expected stream to be finished, got: {next:?}");
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
    async fn test_is_empty_returns_true_when_subscription_none() -> anyhow::Result<()> {
        let (_anvil, provider) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(provider.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .build()
            .await?;

        let mut subscription = robust.subscribe_blocks().await?;

        subscription.subscription = None;
        assert!(subscription.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn test_lag_count_increments_and_resets() -> anyhow::Result<()> {
        // This test verifies the lag counter logic by directly manipulating the internal state
        // In a real scenario, RecvError::Lagged would be triggered by the broadcast channel
        // when the receiver can't keep up with the sender

        let (_anvil, provider) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(provider.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .build()
            .await?;

        let mut subscription = robust.subscribe_blocks().await?;

        // Verify initial state
        assert_eq!(subscription.consecutive_lags, 0);

        // Simulate lag by directly calling process_recv_error
        // In production, this would be called by recv() when the channel lags
        subscription.process_recv_error(RecvError::Lagged(5)).await?;
        assert_eq!(subscription.consecutive_lags, 1, "Lag count should increment to 1");

        // Another lag
        subscription.process_recv_error(RecvError::Lagged(3)).await?;
        assert_eq!(subscription.consecutive_lags, 2, "Lag count should increment to 2");

        // Now receive a successful block - this should reset the counter
        provider.anvil_mine(Some(1), None).await?;
        let block = subscription.recv().await?;
        assert_eq!(block.number, 1);
        assert_eq!(
            subscription.consecutive_lags, 0,
            "Lag count should reset to 0 after successful recv"
        );

        // Verify it stays at 0 for subsequent successful receives
        provider.anvil_mine(Some(1), None).await?;
        let block = subscription.recv().await?;
        assert_eq!(block.number, 2);
        assert_eq!(subscription.consecutive_lags, 0, "Lag count should remain 0");

        Ok(())
    }

    #[tokio::test]
    async fn test_lag_count_triggers_failover_at_max() -> anyhow::Result<()> {
        // Test that MAX_LAG_COUNT consecutive lags trigger a provider switch
        let (_anvil_1, primary) = spawn_ws_anvil().await?;
        let (_anvil_2, fallback) = spawn_ws_anvil().await?;

        let robust = RobustProviderBuilder::fragile(primary.clone())
            .fallback(fallback.clone())
            .subscription_timeout(SHORT_TIMEOUT)
            .build()
            .await?;

        let mut subscription = robust.subscribe_blocks().await?;

        // Verify initial state
        assert_eq!(subscription.consecutive_lags, 0);
        assert_eq!(subscription.current_fallback_index, None, "Should start on primary");

        // Simulate MAX_LAG_COUNT - 1 lags (should NOT trigger failover)
        for i in 1..MAX_LAG_COUNT {
            subscription.process_recv_error(RecvError::Lagged(10)).await?;
            assert_eq!(subscription.consecutive_lags, i, "Lag count should be {i}");
            assert_eq!(subscription.current_fallback_index, None, "Should still be on primary");
        }

        // One more lag should trigger failover
        subscription.process_recv_error(RecvError::Lagged(10)).await?;
        assert_eq!(
            subscription.consecutive_lags, MAX_LAG_COUNT,
            "Lag count should be MAX_LAG_COUNT"
        );
        assert_eq!(
            subscription.current_fallback_index,
            Some(0),
            "Should have failed over to fallback[0]"
        );

        // Verify fallback works
        fallback.anvil_mine(Some(1), None).await?;
        let block = subscription.recv().await?;
        assert_eq!(block.number, 1);
        assert_eq!(
            subscription.consecutive_lags, 0,
            "Lag count should reset after successful recv on fallback"
        );

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
        assert_timeout_error(&mut stream, Duration::ZERO).await;

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
        trigger_failover(&mut stream, fallback.clone(), 1).await?;

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
        assert_timeout_error(&mut stream, SHORT_TIMEOUT).await;

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
        assert_timeout_error(&mut stream, Duration::ZERO).await;

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
}
