use alloy::{
    eips::BlockNumberOrTag,
    network::{Ethereum, Network},
    providers::RootProvider,
    transports::{TransportResult, http::reqwest::Url},
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::{
    EventFilter, Message,
    block_range_scanner::{
        BlockRangeScanner, ConnectedBlockRangeScanner, DEFAULT_BLOCK_CONFIRMATIONS,
        MAX_BUFFERED_MESSAGES,
    },
    event_scanner::listener::EventListener,
};

mod common;
mod historic;
mod latest;
mod live;
mod sync;

#[derive(Default)]
pub struct Unspecified;
pub struct Historic {
    pub(crate) from_block: BlockNumberOrTag,
    pub(crate) to_block: BlockNumberOrTag,
}
pub struct Live {
    pub(crate) block_confirmations: u64,
}
pub struct LatestEvents {
    pub(crate) count: usize,
    pub(crate) from_block: BlockNumberOrTag,
    pub(crate) to_block: BlockNumberOrTag,
    pub(crate) block_confirmations: u64,
}
#[derive(Default)]
pub struct Synchronize;
pub struct SyncFromLatestEvents {
    pub(crate) count: usize,
    pub(crate) block_confirmations: u64,
}
pub struct SyncFromBlock {
    pub(crate) from_block: BlockNumberOrTag,
    pub(crate) block_confirmations: u64,
}

impl Default for Historic {
    fn default() -> Self {
        Self { from_block: BlockNumberOrTag::Earliest, to_block: BlockNumberOrTag::Latest }
    }
}

impl Default for Live {
    fn default() -> Self {
        Self { block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS }
    }
}

pub struct EventScanner<M = Unspecified, N: Network = Ethereum> {
    config: M,
    block_range_scanner: ConnectedBlockRangeScanner<N>,
    listeners: Vec<EventListener>,
}

#[derive(Default)]
pub struct EventScannerBuilder<M, N: Network> {
    pub(crate) config: M,
    pub(crate) block_range_scanner: BlockRangeScanner<N>,
}

impl<N: Network> EventScannerBuilder<Unspecified, N> {
    /// Streams events from a historical block range.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use alloy::network::Ethereum;
    /// # use event_scanner::{EventFilter, EventScannerBuilder, Message};
    /// # use tokio_stream::StreamExt;
    /// #
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let ws_url = "ws://localhost:8545".parse()?;
    /// # let contract_address = alloy::primitives::address!("0xd8dA6BF26964af9d7eed9e03e53415d37aa96045");
    /// // Stream all events from genesis to latest block
    /// let mut scanner = EventScannerBuilder::historic()
    ///     .connect_ws::<Ethereum>(ws_url)
    ///     .await?;
    ///
    /// let filter = EventFilter::new().contract_address(contract_address);
    /// let mut stream = scanner.subscribe(filter);
    ///
    /// scanner.start().await?;
    ///
    /// while let Some(Message::Data(logs)) = stream.next().await {
    ///     println!("Received {} logs", logs.len());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Specifying a custom block range:
    ///
    /// ```no_run
    /// # use alloy::network::Ethereum;
    /// # use event_scanner::EventScannerBuilder;
    /// #
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let ws_url = "ws://localhost:8545".parse()?;
    /// // Stream events between blocks [1_000_000, 2_000_000]
    /// let mut scanner = EventScannerBuilder::historic()
    ///     .from_block(1_000_000)
    ///     .to_block(2_000_000)
    ///     .connect_ws::<Ethereum>(ws_url)
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # How it works
    ///
    /// The scanner streams events in chronological order (oldest to newest) within the specified
    /// block range. Events are delivered in batches as they are fetched from the provider, with
    /// batch sizes controlled by the `max_block_range` configuration.
    ///
    /// # Key behaviors
    ///
    /// - **Continuous streaming**: Events are delivered in multiple messages as they are fetched
    /// - **Chronological order**: Events are always delivered oldest to newest
    /// - **Default range**: By default, scans from `Earliest` to `Latest` block
    /// - **Batch control**: Use `.max_block_range(n)` to control how many blocks are queried per
    ///   RPC call
    /// - **Completion**: The scanner completes when the entire range has been processed
    #[must_use]
    pub fn historic() -> EventScannerBuilder<Historic, N> {
        EventScannerBuilder::default()
    }

    /// Streams new events as blocks are produced on-chain.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use alloy::network::Ethereum;
    /// # use event_scanner::{EventFilter, EventScannerBuilder, Message};
    /// # use tokio_stream::StreamExt;
    /// #
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let ws_url = "ws://localhost:8545".parse()?;
    /// # let contract_address = alloy::primitives::address!("0xd8dA6BF26964af9d7eed9e03e53415d37aa96045");
    /// // Stream new events as they arrive
    /// let mut scanner = EventScannerBuilder::live()
    ///     .block_confirmations(20)
    ///     .connect_ws::<Ethereum>(ws_url)
    ///     .await?;
    ///
    /// let filter = EventFilter::new().contract_address(contract_address);
    /// let mut stream = scanner.subscribe(filter);
    ///
    /// scanner.start().await?;
    ///
    /// while let Some(msg) = stream.next().await {
    ///     match msg {
    ///         Message::Data(logs) => {
    ///             println!("Received {} new events", logs.len());
    ///         }
    ///         Message::Status(status) => {
    ///             println!("Status: {:?}", status);
    ///         }
    ///         Message::Error(e) => {
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
    /// - **Real-time streaming**: Events are delivered as new blocks are confirmed
    /// - **Reorg protection**: Waits for configured confirmations before emitting events
    /// - **Continuous operation**: Runs indefinitely until the scanner is dropped or encounters an
    ///   error
    /// - **Default confirmations**: By default, waits for 12 block confirmations
    ///
    /// # Reorg behavior
    ///
    /// When a reorg is detected:
    /// 1. Emits [`ScannerStatus::ReorgDetected`][reorg] to all listeners
    /// 2. Adjusts the next confirmed range using `block_confirmations`
    /// 3. Re-emits events from the corrected confirmed block range
    /// 4. Continues streaming from the new chain state
    ///
    /// [reorg]: crate::types::ScannerStatus::ReorgDetected
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
    /// See the [sync module documentation](sync) for details on each mode.
    #[must_use]
    pub fn sync() -> EventScannerBuilder<Synchronize> {
        EventScannerBuilder::default()
    }

    /// Streams the latest `count` matching events per registered listener.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use alloy::{network::Ethereum, primitives::Address};
    /// # use event_scanner::{EventFilter, EventScannerBuilder, Message};
    /// # use tokio_stream::StreamExt;
    /// #
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let ws_url = "ws://localhost:8545".parse()?;
    /// # let contract_address = alloy::primitives::address!("0xd8dA6BF26964af9d7eed9e03e53415d37aa96045");
    /// // Collect the latest 10 events across Earliest..=Latest
    /// let mut scanner = EventScannerBuilder::latest(10)
    ///     .connect_ws::<Ethereum>(ws_url)
    ///     .await?;
    ///
    /// let filter = EventFilter::new().contract_address(contract_address);
    /// let mut stream = scanner.subscribe(filter);
    ///
    /// scanner.start().await?;
    ///
    /// // Expect a single message with up to 10 logs, then the stream ends
    /// while let Some(Message::Data(logs)) = stream.next().await {
    ///     println!("Latest logs: {}", logs.len());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Restricting to a specific block range:
    ///
    /// ```no_run
    /// # use alloy::network::Ethereum;
    /// # use event_scanner::EventScannerBuilder;
    /// #
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let ws_url = "ws://localhost:8545".parse()?;
    /// // Collect the latest 5 events between blocks [1_000_000, 1_100_000]
    /// let mut scanner = EventScannerBuilder::latest(5)
    ///     .from_block(1_000_000)
    ///     .to_block(1_100_000)
    ///     .connect_ws::<Ethereum>(ws_url)
    ///     .await?;
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
    /// - **Single delivery**: Each registered stream receives at most `count` logs in a single
    ///   message, chronologically ordered
    /// - **One-shot operation**: The scanner completes after delivering messages; it does not
    ///   continue streaming
    /// - **Flexible count**: If fewer than `count` events exist in the range, returns all available
    ///   events
    /// - **Default range**: By default, scans from `Earliest` to `Latest` block
    /// - **Reorg handling**: Periodically checks the tip to detect reorgs during the scan
    ///
    /// # Arguments
    ///
    /// * `count` - Maximum number of recent events to collect per listener
    ///
    /// # Reorg behavior
    ///
    /// During the scan, the scanner periodically checks the tip to detect reorgs. On reorg
    /// detection:
    /// 1. Emits [`ScannerStatus::ReorgDetected`][reorg] to all listeners
    /// 2. Resets to the updated tip
    /// 3. Restarts the scan from the new tip
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
    /// [reorg]: ScannerStatus::ReorgDetected
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
                from_block: BlockNumberOrTag::Latest,
                to_block: BlockNumberOrTag::Earliest,
                block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS,
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
            },
            block_range_scanner: BlockRangeScanner::default(),
        }
    }
}

impl EventScannerBuilder<SyncFromBlock> {
    #[must_use]
    pub fn new(from_block: BlockNumberOrTag) -> Self {
        Self {
            config: SyncFromBlock { from_block, block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS },
            block_range_scanner: BlockRangeScanner::default(),
        }
    }
}

impl<M> EventScannerBuilder<M> {
    /// Connects to the provider via WebSocket.
    ///
    /// Final builder method: consumes the builder and returns the built [`HistoricEventScanner`].
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ws<N: Network>(self, ws_url: Url) -> TransportResult<EventScanner<M, N>> {
        let block_range_scanner = self.block_range_scanner.connect_ws::<N>(ws_url).await?;
        Ok(EventScanner { config: self.config, block_range_scanner, listeners: Vec::new() })
    }

    /// Connects to the provider via IPC.
    ///
    /// Final builder method: consumes the builder and returns the built [`HistoricEventScanner`].
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<EventScanner<M, N>> {
        let block_range_scanner = self.block_range_scanner.connect_ipc::<N>(ipc_path).await?;
        Ok(EventScanner { config: self.config, block_range_scanner, listeners: Vec::new() })
    }

    /// Connects to an existing provider.
    ///
    /// Final builder method: consumes the builder and returns the built [`HistoricEventScanner`].
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    #[must_use]
    pub fn connect<N: Network>(self, provider: RootProvider<N>) -> EventScanner<M, N> {
        let block_range_scanner = self.block_range_scanner.connect::<N>(provider);
        EventScanner { config: self.config, block_range_scanner, listeners: Vec::new() }
    }
}

impl<M, N: Network> EventScanner<M, N> {
    #[must_use]
    pub fn subscribe(&mut self, filter: EventFilter) -> ReceiverStream<Message> {
        let (sender, receiver) = mpsc::channel::<Message>(MAX_BUFFERED_MESSAGES);
        self.listeners.push(EventListener { filter, sender });
        ReceiverStream::new(receiver)
    }
}

#[cfg(test)]
mod tests {
    use alloy::{providers::mock::Asserter, rpc::client::RpcClient};

    use super::*;

    #[test]
    fn test_historic_scanner_config_defaults() {
        let builder = EventScannerBuilder::<Historic>::default();

        assert!(matches!(builder.config.from_block, BlockNumberOrTag::Earliest));
        assert!(matches!(builder.config.to_block, BlockNumberOrTag::Latest));
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
        assert!(matches!(builder.config.from_block, BlockNumberOrTag::Latest));
        assert!(matches!(builder.config.to_block, BlockNumberOrTag::Earliest));
        assert_eq!(builder.config.block_confirmations, DEFAULT_BLOCK_CONFIRMATIONS);
    }

    #[test]
    fn sync_scanner_config_defaults() {
        let builder = EventScannerBuilder::<SyncFromBlock>::new(BlockNumberOrTag::Earliest);

        assert!(matches!(builder.config.from_block, BlockNumberOrTag::Earliest));
        assert_eq!(builder.config.block_confirmations, DEFAULT_BLOCK_CONFIRMATIONS);
    }

    #[test]
    fn test_historic_event_stream_listeners_vector_updates() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let mut scanner = EventScannerBuilder::historic().connect::<Ethereum>(provider);

        assert!(scanner.listeners.is_empty());

        let _stream1 = scanner.subscribe(EventFilter::new());
        assert_eq!(scanner.listeners.len(), 1);

        let _stream2 = scanner.subscribe(EventFilter::new());
        let _stream3 = scanner.subscribe(EventFilter::new());
        assert_eq!(scanner.listeners.len(), 3);
    }

    #[test]
    fn test_historic_event_stream_channel_capacity() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let mut scanner = EventScannerBuilder::historic().connect::<Ethereum>(provider);

        let _ = scanner.subscribe(EventFilter::new());

        let sender = &scanner.listeners[0].sender;
        assert_eq!(sender.capacity(), MAX_BUFFERED_MESSAGES);
    }
}
