use alloy::{
    eips::{BlockId, BlockNumberOrTag},
    network::{Ethereum, Network},
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::{
    EventFilter, ScannerError,
    block_range_scanner::{
        BlockRangeScanner, ConnectedBlockRangeScanner, DEFAULT_BLOCK_CONFIRMATIONS,
        MAX_BUFFERED_MESSAGES, RingBufferCapacity,
    },
    event_scanner::{EventScannerResult, listener::EventListener},
    robust_provider::IntoRobustProvider,
};

mod common;
mod historic;
mod latest;
mod live;
mod sync;

/// Default number of maximum concurrent fetches for each scanner mode.
pub const DEFAULT_MAX_CONCURRENT_FETCHES: usize = 24;

#[derive(Default)]
pub struct Unspecified;
pub struct Historic {
    pub(crate) from_block: BlockId,
    pub(crate) to_block: BlockId,
    /// Controls how many log-fetching RPC requests can run in parallel during the scan.
    pub(crate) max_concurrent_fetches: usize,
}
pub struct Live {
    pub(crate) block_confirmations: u64,
    /// Controls how many log-fetching RPC requests can run in parallel during the scan.
    pub(crate) max_concurrent_fetches: usize,
}
pub struct LatestEvents {
    pub(crate) count: usize,
    pub(crate) from_block: BlockId,
    pub(crate) to_block: BlockId,
    pub(crate) block_confirmations: u64,
    /// Controls how many log-fetching RPC requests can run in parallel during the scan.
    pub(crate) max_concurrent_fetches: usize,
}
#[derive(Default)]
pub struct Synchronize;
pub struct SyncFromLatestEvents {
    pub(crate) count: usize,
    pub(crate) block_confirmations: u64,
    /// Controls how many log-fetching RPC requests can run in parallel during the scan.
    pub(crate) max_concurrent_fetches: usize,
}
pub struct SyncFromBlock {
    pub(crate) from_block: BlockId,
    pub(crate) block_confirmations: u64,
    /// Controls how many log-fetching RPC requests can run in parallel during the scan.
    pub(crate) max_concurrent_fetches: usize,
}

impl Default for Historic {
    fn default() -> Self {
        Self {
            from_block: BlockNumberOrTag::Earliest.into(),
            to_block: BlockNumberOrTag::Latest.into(),
            max_concurrent_fetches: DEFAULT_MAX_CONCURRENT_FETCHES,
        }
    }
}

impl Default for Live {
    fn default() -> Self {
        Self {
            block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS,
            max_concurrent_fetches: DEFAULT_MAX_CONCURRENT_FETCHES,
        }
    }
}

pub struct EventScanner<M = Unspecified, N: Network = Ethereum> {
    config: M,
    block_range_scanner: ConnectedBlockRangeScanner<N>,
    listeners: Vec<EventListener>,
}

#[derive(Default)]
pub struct EventScannerBuilder<M> {
    pub(crate) config: M,
    pub(crate) block_range_scanner: BlockRangeScanner,
}

impl EventScannerBuilder<Unspecified> {
    /// Streams events from a historical block range.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use alloy::{network::Ethereum, providers::{Provider, ProviderBuilder}};
    /// # use event_scanner::{EventFilter, EventScannerBuilder, Message, robust_provider::RobustProviderBuilder};
    /// # use tokio_stream::StreamExt;
    /// #
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let contract_address = alloy::primitives::address!("0xd8dA6BF26964af9d7eed9e03e53415d37aa96045");
    /// // Stream all events from genesis to latest block
    /// let provider = ProviderBuilder::new().connect("ws://localhost:8545").await?;
    /// let robust_provider = RobustProviderBuilder::new(provider).build().await?;
    /// let mut scanner = EventScannerBuilder::historic().connect(robust_provider).await?;
    ///
    /// let filter = EventFilter::new().contract_address(contract_address);
    /// let mut stream = scanner.subscribe(filter);
    ///
    /// scanner.start().await?;
    ///
    /// while let Some(Ok(Message::Data(logs))) = stream.next().await {
    ///     println!("Received {} logs", logs.len());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Specifying a custom block range:
    ///
    /// ```no_run
    /// # use alloy::{network::Ethereum, providers::{Provider, ProviderBuilder}};
    /// # use event_scanner::{EventScannerBuilder, robust_provider::RobustProviderBuilder};
    /// #
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// // Stream events between blocks [1_000_000, 2_000_000]
    /// let provider = ProviderBuilder::new().connect("ws://localhost:8545").await?;
    /// let robust_provider = RobustProviderBuilder::new(provider).build().await?;
    /// let mut scanner = EventScannerBuilder::historic()
    ///     .from_block(1_000_000)
    ///     .to_block(2_000_000)
    ///     .connect(robust_provider)
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # How it works
    ///
    /// The scanner streams events in chronological order (oldest to newest) within the specified
    /// block range. Events are delivered in batches as they are fetched from the provider, with
    /// batch sizes controlled by the [`max_block_range`][max_block_range] configuration.
    ///
    /// # Key behaviors
    ///
    /// * **Continuous streaming**: Events are delivered in multiple messages as they are fetched
    /// * **Chronological order**: Events are always delivered oldest to newest
    /// * **Concurrent log fetching**: Logs are fetched concurrently to reduce the execution time.
    ///   The maximum number of concurrent RPC calls is controlled by
    ///   [`max_concurrent_fetches`][max_concurrent_fetches]
    /// * **Default range**: By default, scans from `Earliest` to `Latest` block
    /// * **Batch control**: Use [`max_block_range`][max_block_range] to control how many blocks are
    ///   queried per RPC call
    /// * **Reorg handling**: Performs reorg checks when streaming events from non-finalized blocks;
    ///   if a reorg is detected, streams events from the reorged blocks
    /// * **Completion**: The scanner completes when the entire range has been processed.
    ///
    /// [max_block_range]: crate::EventScannerBuilder::max_block_range
    /// [max_concurrent_fetches]: crate::EventScannerBuilder::max_concurrent_fetches
    #[must_use]
    pub fn historic() -> EventScannerBuilder<Historic> {
        EventScannerBuilder::default()
    }

    /// Streams new events as blocks are produced on-chain.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use alloy::{network::Ethereum, providers::{Provider, ProviderBuilder}};
    /// # use event_scanner::{EventFilter, EventScannerBuilder, Message, robust_provider::RobustProviderBuilder};
    /// # use tokio_stream::StreamExt;
    /// #
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let contract_address = alloy::primitives::address!("0xd8dA6BF26964af9d7eed9e03e53415d37aa96045");
    /// // Stream new events as they arrive
    /// let provider = ProviderBuilder::new().connect("ws://localhost:8545").await?;
    /// let robust_provider = RobustProviderBuilder::new(provider).build().await?;
    /// let mut scanner = EventScannerBuilder::live()
    ///     .block_confirmations(20)
    ///     .connect(robust_provider)
    ///     .await?;
    ///
    /// let filter = EventFilter::new().contract_address(contract_address);
    /// let mut stream = scanner.subscribe(filter);
    ///
    /// scanner.start().await?;
    ///
    /// while let Some(msg) = stream.next().await {
    ///     match msg {
    ///         Ok(Message::Data(logs)) => {
    ///             println!("Received {} new events", logs.len());
    ///         }
    ///         Ok(Message::Notification(notification)) => {
    ///             println!("Notification received: {:?}", notification);
    ///         }
    ///         Err(e) => {
    ///             eprintln!("Error: {}", e);
    ///         }
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # How it works
    ///
    /// The scanner subscribes to new blocks via WebSocket and streams events from confirmed
    /// blocks. The `block_confirmations` setting determines how many blocks to wait before
    /// considering a block confirmed, providing protection against chain reorganizations.
    ///
    /// # Key behaviors
    ///
    /// * **Real-time streaming**: Events are delivered as new blocks are confirmed
    /// * **Reorg protection**: Waits for configured confirmations before emitting events
    /// * **Continuous operation**: Runs indefinitely until the scanner is dropped or encounters an
    ///   error
    /// * **Default confirmations**: By default, waits for 12 block confirmations
    ///
    /// # Reorg behavior
    ///
    /// When a reorg is detected:
    /// 1. Emits [`Notification::ReorgDetected`][reorg] to all listeners
    /// 2. Adjusts the next confirmed range using `block_confirmations`
    /// 3. Re-emits events from the corrected confirmed block range
    /// 4. Continues streaming from the new chain state
    ///
    /// [reorg]: crate::types::Notification::ReorgDetected
    #[must_use]
    pub fn live() -> EventScannerBuilder<Live> {
        EventScannerBuilder::default()
    }

    /// Creates a builder for sync mode scanners that combine historical catch-up with live
    /// streaming.
    ///
    /// This method returns a builder that must be further narrowed down:
    /// ```rust,no_run
    /// # use event_scanner::EventScannerBuilder;
    /// // Sync from block mode
    /// EventScannerBuilder::sync().from_block(1_000_000);
    /// // Sync from latest events mode
    /// EventScannerBuilder::sync().from_latest(10);
    /// ```
    ///
    /// See [`from_block`](crate::EventScannerBuilder#method.from_block-2) and
    /// [`from_latest`](crate::EventScannerBuilder#method.from_latest) for details on each mode.
    #[must_use]
    pub fn sync() -> EventScannerBuilder<Synchronize> {
        EventScannerBuilder::default()
    }

    /// Streams the latest `count` matching events per registered listener.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use alloy::{network::Ethereum, providers::{Provider, ProviderBuilder}};
    /// # use event_scanner::{EventFilter, EventScannerBuilder, Message, robust_provider::RobustProviderBuilder};
    /// # use tokio_stream::StreamExt;
    /// #
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let contract_address = alloy::primitives::address!("0xd8dA6BF26964af9d7eed9e03e53415d37aa96045");
    /// // Collect the latest 10 events across Earliest..=Latest
    /// let provider = ProviderBuilder::new().connect("ws://localhost:8545").await?;
    /// let robust_provider = RobustProviderBuilder::new(provider).build().await?;
    /// let mut scanner = EventScannerBuilder::latest(10).connect(robust_provider).await?;
    ///
    /// let filter = EventFilter::new().contract_address(contract_address);
    /// let mut stream = scanner.subscribe(filter);
    ///
    /// scanner.start().await?;
    ///
    /// // Expect a single message with up to 10 logs, then the stream ends
    /// while let Some(Ok(Message::Data(logs))) = stream.next().await {
    ///     println!("Latest logs: {}", logs.len());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Restricting to a specific block range:
    ///
    /// ```no_run
    /// # use alloy::{network::Ethereum, providers::{Provider, ProviderBuilder}};
    /// # use event_scanner::{EventScannerBuilder, robust_provider::RobustProviderBuilder};
    /// #
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// // Collect the latest 5 events between blocks [1_000_000, 1_100_000]
    /// let provider = ProviderBuilder::new().connect("ws://localhost:8545").await?;
    /// let robust_provider = RobustProviderBuilder::new(provider).build().await?;
    /// let mut scanner = EventScannerBuilder::latest(5)
    ///     .from_block(1_000_000)
    ///     .to_block(1_100_000)
    ///     .connect(robust_provider);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # How it works
    ///
    /// The scanner performs a reverse-ordered scan (newest to oldest) within the specified block
    /// range, collecting up to `count` events per registered listener. Once the target count is
    /// reached or the range is exhausted, it delivers the events in chronological order (oldest to
    /// newest) and completes.
    ///
    /// When using a custom block range, the scanner automatically normalizes the range boundaries.
    /// This means you can specify `from_block` and `to_block` in any order - the scanner will
    /// always scan from the higher block number down to the lower one, regardless of which
    /// parameter holds which value.
    ///
    /// # Key behaviors
    ///
    /// * **Single delivery**: Each registered stream receives at most `count` logs in a single
    ///   message, chronologically ordered
    /// * **One-shot operation**: The scanner completes after delivering messages; it does not
    ///   continue streaming
    /// * **Concurrent log fetching**: Logs are fetched concurrently to reduce the execution time.
    ///   The maximum number of concurrent RPC calls is controlled by
    ///   [`max_concurrent_fetches`][max_concurrent_fetches]
    /// * **Flexible count**: If fewer than `count` events exist in the range, returns all available
    ///   events
    /// * **Default range**: By default, scans from `Earliest` to `Latest` block
    /// * **Reorg handling**: Periodically checks the tip to detect reorgs during the scan
    ///
    /// # Notifications
    ///
    /// The scanner can emit the following notifications:
    ///
    /// * [`Notification::NoPastLogsFound`][no_logs]: Emitted when no matching logs are found in the
    ///   scanned range.
    /// * [`Notification::ReorgDetected`][reorg]: Emitted when a reorg is detected during the scan.
    ///
    /// # Arguments
    ///
    /// * `count` - Maximum number of recent events to collect per listener (must be greater than 0)
    ///
    /// # Reorg behavior
    ///
    /// The scanner can detect reorgs during the scan by periodically checking that the range tip
    /// has not changed. This is done only when the specified range tip is not a finalized
    /// block.
    ///
    /// On reorg detection:
    /// 1. Emits [`Notification::ReorgDetected`][reorg] to all listeners
    /// 2. Resets to the updated tip
    /// 3. Reloads logs from the block range affected by the reorg
    /// 4. Continues until `count` events are collected
    ///
    /// Final delivery to log listeners preserves chronological order regardless of reorgs.
    ///
    /// # Notes
    ///
    /// For continuous streaming after collecting latest events, use
    /// [`EventScannerBuilder::sync().from_latest(count)`][sync_from_latest] instead
    ///
    /// [subscribe]: EventScanner::subscribe
    /// [start]: EventScanner::start
    /// [sync_from_latest]: EventScannerBuilder::from_latest
    /// [reorg]: crate::Notification::ReorgDetected
    /// [no_logs]: crate::Notification::NoPastLogsFound
    /// [max_concurrent_fetches]: crate::EventScannerBuilder#method.max_concurrent_fetches-1
    #[must_use]
    pub fn latest(count: usize) -> EventScannerBuilder<LatestEvents> {
        EventScannerBuilder::<LatestEvents>::new(count)
    }
}

impl EventScannerBuilder<LatestEvents> {
    #[must_use]
    pub fn new(count: usize) -> Self {
        Self {
            config: LatestEvents {
                count,
                from_block: BlockNumberOrTag::Latest.into(),
                to_block: BlockNumberOrTag::Earliest.into(),
                block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS,
                max_concurrent_fetches: DEFAULT_MAX_CONCURRENT_FETCHES,
            },
            block_range_scanner: BlockRangeScanner::default(),
        }
    }
}

impl EventScannerBuilder<SyncFromLatestEvents> {
    #[must_use]
    pub fn new(count: usize) -> Self {
        Self {
            config: SyncFromLatestEvents {
                count,
                block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS,
                max_concurrent_fetches: DEFAULT_MAX_CONCURRENT_FETCHES,
            },
            block_range_scanner: BlockRangeScanner::default(),
        }
    }
}

impl EventScannerBuilder<SyncFromBlock> {
    #[must_use]
    pub fn new(from_block: impl Into<BlockId>) -> Self {
        Self {
            config: SyncFromBlock {
                from_block: from_block.into(),
                block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS,
                max_concurrent_fetches: DEFAULT_MAX_CONCURRENT_FETCHES,
            },
            block_range_scanner: BlockRangeScanner::default(),
        }
    }
}

impl<M> EventScannerBuilder<M> {
    /// Sets the maximum block range per event batch.
    ///
    /// Controls how the scanner splits a large block range into smaller batches for processing.
    /// Each batch corresponds to a single RPC call to fetch logs. This prevents timeouts and
    /// respects rate limits imposed by node providers.
    ///
    /// # Arguments
    ///
    /// * `max_block_range` - Maximum number of blocks to process per batch (must be greater than 0)
    ///
    /// # Example
    ///
    /// If scanning events from blocks 1000–1099 (100 blocks total) with `max_block_range(30)`:
    /// * Batch 1: blocks 1000–1029 (30 blocks)
    /// * Batch 2: blocks 1030–1059 (30 blocks)
    /// * Batch 3: blocks 1060–1089 (30 blocks)
    /// * Batch 4: blocks 1090–1099 (10 blocks)
    #[must_use]
    pub fn max_block_range(mut self, max_block_range: u64) -> Self {
        self.block_range_scanner.max_block_range = max_block_range;
        self
    }

    /// Sets how many of past blocks to keep in memory for reorg detection.
    ///
    /// IMPORTANT: If zero, reorg detection is disabled.
    ///
    /// # Arguments
    ///
    /// * `past_blocks_storage_capacity` - Maximum number of blocks to keep in memory.
    #[must_use]
    pub fn past_blocks_storage_capacity(
        mut self,
        past_blocks_storage_capacity: RingBufferCapacity,
    ) -> Self {
        self.block_range_scanner.past_blocks_storage_capacity = past_blocks_storage_capacity;
        self
    }

    /// Builds the scanner by connecting to an existing provider.
    ///
    /// This is a shared method used internally by scanner-specific `connect()` methods.
    async fn build<N: Network>(
        self,
        provider: impl IntoRobustProvider<N>,
    ) -> Result<EventScanner<M, N>, ScannerError> {
        if self.block_range_scanner.max_block_range == 0 {
            return Err(ScannerError::InvalidMaxBlockRange);
        }
        let block_range_scanner = self.block_range_scanner.connect::<N>(provider).await?;
        Ok(EventScanner { config: self.config, block_range_scanner, listeners: Vec::new() })
    }
}

impl<M, N: Network> EventScanner<M, N> {
    #[must_use]
    pub fn subscribe(&mut self, filter: EventFilter) -> ReceiverStream<EventScannerResult> {
        let (sender, receiver) = mpsc::channel::<EventScannerResult>(MAX_BUFFERED_MESSAGES);
        self.listeners.push(EventListener { filter, sender });
        ReceiverStream::new(receiver)
    }
}

#[cfg(test)]
mod tests {
    use alloy::{
        providers::{RootProvider, mock::Asserter},
        rpc::client::RpcClient,
    };

    use super::*;

    #[test]
    fn test_historic_scanner_config_defaults() {
        let builder = EventScannerBuilder::<Historic>::default();

        assert_eq!(builder.config.from_block, BlockNumberOrTag::Earliest.into());
        assert_eq!(builder.config.to_block, BlockNumberOrTag::Latest.into());
    }

    #[test]
    fn test_live_scanner_config_defaults() {
        let builder = EventScannerBuilder::<Live>::default();

        assert_eq!(builder.config.block_confirmations, DEFAULT_BLOCK_CONFIRMATIONS);
    }

    #[test]
    fn test_latest_scanner_config_defaults() {
        let builder = EventScannerBuilder::<LatestEvents>::new(10);

        assert_eq!(builder.config.count, 10);

        assert_eq!(builder.config.from_block, BlockNumberOrTag::Latest.into());
        assert_eq!(builder.config.to_block, BlockNumberOrTag::Earliest.into());
        assert_eq!(builder.config.block_confirmations, DEFAULT_BLOCK_CONFIRMATIONS);
    }

    #[test]
    fn sync_scanner_config_defaults() {
        let builder = EventScannerBuilder::<SyncFromBlock>::new(BlockNumberOrTag::Earliest);

        assert_eq!(builder.config.from_block, BlockNumberOrTag::Earliest.into());
        assert_eq!(builder.config.block_confirmations, DEFAULT_BLOCK_CONFIRMATIONS);
    }

    #[tokio::test]
    async fn test_historic_event_stream_listeners_vector_updates() -> anyhow::Result<()> {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let mut scanner = EventScannerBuilder::historic().build(provider).await?;

        assert!(scanner.listeners.is_empty());

        let _stream1 = scanner.subscribe(EventFilter::new());
        assert_eq!(scanner.listeners.len(), 1);

        let _stream2 = scanner.subscribe(EventFilter::new());
        let _stream3 = scanner.subscribe(EventFilter::new());
        assert_eq!(scanner.listeners.len(), 3);

        Ok(())
    }

    #[tokio::test]
    async fn test_historic_event_stream_channel_capacity() -> anyhow::Result<()> {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let mut scanner = EventScannerBuilder::historic().build(provider).await?;

        let _ = scanner.subscribe(EventFilter::new());

        let sender = &scanner.listeners[0].sender;
        assert_eq!(sender.capacity(), MAX_BUFFERED_MESSAGES);

        Ok(())
    }

    #[tokio::test]
    async fn test_latest_returns_error_with_zero_count() {
        use alloy::{
            providers::{RootProvider, mock::Asserter},
            rpc::client::RpcClient,
        };

        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let result = EventScannerBuilder::latest(0).connect(provider).await;

        match result {
            Err(ScannerError::InvalidEventCount) => {}
            _ => panic!("Expected InvalidEventCount error"),
        }
    }
}
