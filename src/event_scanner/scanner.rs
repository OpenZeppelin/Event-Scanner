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

impl Default for LatestEvents {
    fn default() -> Self {
        Self {
            count: 1,
            from_block: BlockNumberOrTag::Latest,
            to_block: BlockNumberOrTag::Earliest,
            block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS,
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
    #[must_use]
    pub fn historic() -> EventScannerBuilder<Historic> {
        Default::default()
    }

    #[must_use]
    pub fn live() -> EventScannerBuilder<Live> {
        Default::default()
    }

    #[must_use]
    pub fn sync() -> EventScannerBuilder<Synchronize> {
        Default::default()
    }

    /// Streams the latest `count` matching events per registered listener.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use alloy::{network::Ethereum, primitives::Address};
    /// # use event_scanner::{EventFilter, EventScanner, Message};
    /// # use tokio_stream::StreamExt;
    /// #
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let ws_url = "ws://localhost:8545".parse()?;
    /// # let contract_address = alloy::primitives::address!("0xd8dA6BF26964af9d7eed9e03e53415d37aa96045");
    /// // Collect the latest 10 events across Earliest..=Latest
    /// let mut scanner = EventScanner::latest()
    ///     .count(10)
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
    /// # use event_scanner::EventScanner;
    /// #
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let ws_url = "ws://localhost:8545".parse()?;
    /// // Collect the latest 5 events between blocks [1_000_000, 1_100_000]
    /// let mut scanner = EventScanner::latest()
    ///     .count(5)
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
    /// # Important notes
    ///
    /// - Register event streams via [`scanner.subscribe(filter)`][subscribe] **before** calling
    ///   [`scanner.start()`][start]
    /// - The [`scanner.start()`][start] method returns immediately; events are delivered
    ///   asynchronously
    /// - For continuous streaming after collecting latest events, use
    ///   [`EventScanner::sync().from_latest(count)`][sync_from_latest] instead
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
    /// [count]: latest::LatestScannerBuilder::count
    /// [from_block]: latest::LatestScannerBuilder::from_block
    /// [to_block]: latest::LatestScannerBuilder::to_block
    /// [block_confirmations]: latest::LatestScannerBuilder::block_confirmations
    /// [max_block_range]: latest::LatestScannerBuilder::max_block_range
    /// [subscribe]: latest::LatestEventScanner::subscribe
    /// [start]: latest::LatestEventScanner::start
    /// [sync_from_latest]: SyncScannerBuilder::from_latest
    /// [reorg]: crate::types::ScannerStatus::ReorgDetected
    #[must_use]
    pub fn latest() -> EventScannerBuilder<LatestEvents> {
        Default::default()
    }
}

impl EventScannerBuilder<SyncFromLatestEvents> {
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
        let builder = EventScannerBuilder::historic();

        assert!(matches!(builder.config.from_block, BlockNumberOrTag::Earliest));
        assert!(matches!(builder.config.to_block, BlockNumberOrTag::Latest));
    }

    #[test]
    fn test_live_scanner_config_defaults() {
        let builder = EventScannerBuilder::live();

        assert_eq!(builder.config.block_confirmations, DEFAULT_BLOCK_CONFIRMATIONS);
    }

    #[test]
    fn test_latest_scanner_config_defaults() {
        let builder = EventScannerBuilder::latest();

        assert_eq!(builder.config.count, 1);
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
