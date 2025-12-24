use alloy::{eips::BlockNumberOrTag, network::Network};

use crate::{
    EventScannerBuilder, ScannerError,
    event_scanner::{
        EventScanner,
        scanner::{
            SyncFromLatestEvents,
            common::{ConsumerMode, handle_stream},
        },
    },
    robust_provider::IntoRobustProvider,
    types::TryStream,
};

impl EventScannerBuilder<SyncFromLatestEvents> {
    /// Sets the number of confirmations required before a block is considered stable enough to
    /// scan in the live phase.
    ///
    /// This affects the post-sync live streaming phase; higher values reduce reorg risk at the
    /// cost of increased event delivery latency.
    #[must_use]
    pub fn block_confirmations(mut self, confirmations: u64) -> Self {
        self.config.block_confirmations = confirmations;
        self
    }

    /// Sets the maximum number of block-range fetches to process concurrently when
    /// fetching the latest events before switching to live streaming.
    ///
    /// Increasing this value can improve catch-up throughput by issuing multiple
    /// RPC requests concurrently, at the cost of additional load on the provider.
    ///
    /// Must be greater than 0.
    ///
    /// Defaults to [`DEFAULT_MAX_CONCURRENT_FETCHES`][default].
    ///
    /// [default]: crate::event_scanner::scanner::DEFAULT_MAX_CONCURRENT_FETCHES
    #[must_use]
    pub fn max_concurrent_fetches(mut self, max_concurrent_fetches: usize) -> Self {
        self.config.max_concurrent_fetches = max_concurrent_fetches;
        self
    }

    /// Connects to an existing provider.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// * The provider connection fails
    /// * The event count is zero
    /// * The max block range is zero
    pub async fn connect<N: Network>(
        self,
        provider: impl IntoRobustProvider<N>,
    ) -> Result<EventScanner<SyncFromLatestEvents, N>, ScannerError> {
        if self.config.count == 0 {
            return Err(ScannerError::InvalidEventCount);
        }
        if self.config.max_concurrent_fetches == 0 {
            return Err(ScannerError::InvalidMaxConcurrentFetches);
        }
        self.build(provider).await
    }
}

impl<N: Network> EventScanner<SyncFromLatestEvents, N> {
    /// Starts the scanner in [`SyncFromLatestEvents`] mode.
    ///
    /// See [`EventScanner`] for general startup notes.
    ///
    /// # Errors
    ///
    /// * [`ScannerError::Timeout`] - if an RPC call required for startup times out.
    /// * [`ScannerError::RpcError`] - if an RPC call required for startup fails.
    #[allow(clippy::missing_panics_doc)]
    pub async fn start(self) -> Result<(), ScannerError> {
        info!(
            event_count = self.config.count,
            block_confirmations = self.config.block_confirmations,
            listener_count = self.listeners.len(),
            "Starting EventScanner in SyncFromLatestEvents mode"
        );

        let count = self.config.count;
        let provider = self.block_range_scanner.provider().clone();
        let listeners = self.listeners.clone();
        let max_concurrent_fetches = self.config.max_concurrent_fetches;
        let buffer_capacity = self.buffer_capacity();

        // Fetch the latest block number.
        // This is used to determine the starting point for the rewind stream and the live
        // stream. We do this before starting the streams to avoid a race condition
        // where the latest block changes while we're setting up the streams.
        let latest_block = provider.get_block_number().await?;

        // Setup rewind and live streams to run in parallel.
        let rewind_stream = self
            .block_range_scanner
            .stream_rewind(latest_block, BlockNumberOrTag::Earliest)
            .await?;

        // Start streaming...
        tokio::spawn(async move {
            debug!(
                latest_block = latest_block,
                count = count,
                "Phase 1: Collecting latest events via rewind"
            );

            // Since both rewind and live log consumers are ultimately streaming to the same
            // channel, we must ensure that all latest events are streamed before
            // consuming the live stream, otherwise the log consumers may send events out
            // of order.
            handle_stream(
                rewind_stream,
                &provider,
                &listeners,
                ConsumerMode::CollectLatest { count },
                max_concurrent_fetches,
                buffer_capacity,
            )
            .await;

            debug!(
                start_block = latest_block + 1,
                "Phase 2: Catching up and transitioning to live mode"
            );

            // We actually rely on the sync mode for the live stream, as more blocks could have been
            // minted while the scanner was collecting the latest `count` events.
            // Note: Sync mode will notify the client when it switches to live streaming.
            let sync_stream = match self
                .block_range_scanner
                .stream_from(latest_block + 1, self.config.block_confirmations)
                .await
            {
                Ok(stream) => stream,
                Err(e) => {
                    error!("Failed to setup sync stream after collecting latest events");
                    for listener in listeners {
                        _ = listener.sender.try_stream(e.clone()).await;
                    }
                    return;
                }
            };

            // Start the live (sync) stream.
            handle_stream(
                sync_stream,
                &provider,
                &listeners,
                ConsumerMode::Stream,
                max_concurrent_fetches,
                buffer_capacity,
            )
            .await;

            debug!("SyncFromLatestEvents stream ended");
        });

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use alloy::{
        network::Ethereum,
        providers::{ProviderBuilder, RootProvider, mock::Asserter},
        rpc::client::RpcClient,
    };
    use alloy_node_bindings::Anvil;

    use crate::{
        block_range_scanner::{
            DEFAULT_BLOCK_CONFIRMATIONS, DEFAULT_MAX_BLOCK_RANGE, DEFAULT_STREAM_BUFFER_CAPACITY,
        },
        event_scanner::scanner::DEFAULT_MAX_CONCURRENT_FETCHES,
    };

    use super::*;

    #[test]
    fn builder_pattern() {
        let builder = EventScannerBuilder::sync()
            .from_latest(1)
            .block_confirmations(2)
            .max_block_range(50)
            .max_concurrent_fetches(10)
            .buffer_capacity(33);

        assert_eq!(builder.config.count, 1);
        assert_eq!(builder.config.block_confirmations, 2);
        assert_eq!(builder.block_range_scanner.max_block_range, 50);
        assert_eq!(builder.config.max_concurrent_fetches, 10);
        assert_eq!(builder.block_range_scanner.buffer_capacity, 33);
    }

    #[test]
    fn builder_with_default_values() {
        let builder = EventScannerBuilder::sync().from_latest(1);

        assert_eq!(builder.config.count, 1);
        assert_eq!(builder.config.block_confirmations, DEFAULT_BLOCK_CONFIRMATIONS);
        assert_eq!(builder.block_range_scanner.max_block_range, DEFAULT_MAX_BLOCK_RANGE);
        assert_eq!(builder.config.max_concurrent_fetches, DEFAULT_MAX_CONCURRENT_FETCHES);
        assert_eq!(builder.block_range_scanner.buffer_capacity, DEFAULT_STREAM_BUFFER_CAPACITY);
    }

    #[test]
    fn builder_last_call_wins() {
        let builder = EventScannerBuilder::sync()
            .from_latest(1)
            .max_block_range(25)
            .max_block_range(55)
            .max_block_range(105)
            .block_confirmations(2)
            .block_confirmations(3)
            .max_concurrent_fetches(10)
            .max_concurrent_fetches(20)
            .buffer_capacity(20)
            .buffer_capacity(40);

        assert_eq!(builder.config.count, 1);
        assert_eq!(builder.block_range_scanner.max_block_range, 105);
        assert_eq!(builder.config.block_confirmations, 3);
        assert_eq!(builder.config.max_concurrent_fetches, 20);
        assert_eq!(builder.block_range_scanner.buffer_capacity, 40);
    }

    #[tokio::test]
    async fn accepts_zero_confirmations() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn().unwrap();
        let provider = ProviderBuilder::new().connect_http(anvil.endpoint_url());

        let scanner = EventScannerBuilder::sync()
            .from_latest(1)
            .block_confirmations(0)
            .connect(provider)
            .await?;

        assert_eq!(scanner.config.block_confirmations, 0);

        Ok(())
    }

    #[tokio::test]
    async fn returns_error_with_zero_max_concurrent_fetches() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let result = EventScannerBuilder::sync()
            .from_latest(1)
            .max_concurrent_fetches(0)
            .connect(provider)
            .await;

        assert!(matches!(result, Err(ScannerError::InvalidMaxConcurrentFetches)));
    }

    #[tokio::test]
    async fn test_sync_from_latest_returns_error_with_zero_count() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let result = EventScannerBuilder::sync().from_latest(0).connect(provider).await;

        match result {
            Err(ScannerError::InvalidEventCount) => {}
            _ => panic!("Expected InvalidEventCount error"),
        }
    }

    #[tokio::test]
    async fn test_sync_from_latest_returns_error_with_zero_max_block_range() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let result =
            EventScannerBuilder::sync().from_latest(10).max_block_range(0).connect(provider).await;

        match result {
            Err(ScannerError::InvalidMaxBlockRange) => {}
            _ => panic!("Expected InvalidMaxBlockRange error"),
        }
    }

    #[tokio::test]
    async fn returns_error_with_zero_buffer_capacity() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let result =
            EventScannerBuilder::sync().from_latest(10).buffer_capacity(0).connect(provider).await;

        assert!(matches!(result, Err(ScannerError::InvalidBufferCapacity)));
    }
}
