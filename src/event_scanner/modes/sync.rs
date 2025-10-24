use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    providers::RootProvider,
    transports::{TransportResult, http::reqwest::Url},
};

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::{
    ScannerError,
    block_range_scanner::{
        BlockRangeScanner, ConnectedBlockRangeScanner, DEFAULT_BLOCK_CONFIRMATIONS,
        MAX_BUFFERED_MESSAGES,
    },
    event_scanner::{
        filter::EventFilter,
        listener::EventListener,
        message::Message,
        modes::common::{ConsumerMode, handle_stream},
    },
};

pub(crate) mod from_latest;

use from_latest::SyncFromLatestScannerBuilder;

pub struct SyncScannerBuilder {
    block_range_scanner: BlockRangeScanner,
    from_block: BlockNumberOrTag,
    block_confirmations: u64,
}

pub struct SyncEventScanner<N: Network> {
    config: SyncScannerBuilder,
    block_range_scanner: ConnectedBlockRangeScanner<N>,
    listeners: Vec<EventListener>,
}

impl SyncScannerBuilder {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            block_range_scanner: BlockRangeScanner::new(),
            from_block: BlockNumberOrTag::Earliest,
            block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS,
        }
    }

    /// Scans the latest `count` matching events per registered listener, then automatically
    /// transitions to live streaming mode.
    ///
    /// This method combines two scanning phases into a single operation:
    /// 1. **Latest events phase**: Collects up to `count` most recent events by scanning backwards
    ///    from the current chain tip
    /// 2. **Live streaming phase**: Continuously monitors and streams new events as they arrive
    ///    on-chain
    ///
    /// # Two-Phase Operation
    ///
    /// The method captures the latest block number before starting both phases to establish a
    /// clear boundary. The historical phase scans from `Earliest` to `latest_block`, while the
    /// live phase uses sync mode starting from `latest_block + 1`. This design prevents duplicate
    /// events and handles race conditions where new blocks arrive during setup.
    ///
    /// Between phases, the scanner emits [`ScannerStatus::SwitchingToLive`] to notify listeners
    /// of the transition. As previously mentioned, the live phase internally uses sync mode
    /// (historical → live) to ensure no events are missed if blocks were mined during the
    /// transition or if reorgs occur.
    ///
    /// # Arguments
    ///
    /// * `count` - Maximum number of recent events to collect per listener before switching to
    ///   live.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use alloy::network::Ethereum;
    /// # use event_scanner::{EventFilter, EventScanner, EventScannerMessage};
    /// # use tokio_stream::StreamExt;
    /// #
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let ws_url = "ws://localhost:8545".parse()?;
    /// # let contract_address = alloy::primitives::address!("0xd8dA6BF26964af9d7eed9e03e53415d37aa96045");
    /// let mut scanner = EventScanner::sync()
    ///     .from_latest(10)
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
    ///         EventScannerMessage::Data(logs) => {
    ///             println!("Received {} events", logs.len());
    ///         }
    ///         EventScannerMessage::Status(status) => {
    ///             println!("Status: {:?}", status);
    ///         }
    ///         EventScannerMessage::Error(e) => {
    ///             eprintln!("Error: {}", e);
    ///         }
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Edge Cases
    ///
    /// - **No historical events**: If fewer than `count` events exist (or none at all), the method
    ///   returns all available events, then transitions to live streaming normally.
    /// - **Duplicate prevention**: The boundary at `latest_block` ensures events are never
    ///   delivered twice across the phase transition.
    /// - **Race conditions**: Fetching `latest_block` before setting up streams prevents missing
    ///   events that arrive during initialization.
    ///
    /// # Reorg Behavior
    ///
    /// - **Historical rewind phase**: Reverse-ordered rewind over `Earliest..=latest_block`. On
    ///   detecting a reorg, emits [`ScannerStatus::ReorgDetected`], resets the rewind start to the
    ///   new tip, and continues until collectors accumulate `count` logs. Final delivery to
    ///   listeners preserves chronological order.
    /// - **Live streaming phase**: Starts from `latest_block + 1` and respects block confirmations
    ///   configured via [`with_block_confirmations`](Self::with_block_confirmations). On reorg,
    ///   emits [`ScannerStatus::ReorgDetected`], adjusts the next confirmed window (possibly
    ///   re-emitting confirmed portions), and continues streaming.
    ///
    /// # Usage Notes
    ///
    /// - Call [`subscribe`](Self::subscribe) to register listeners **before** calling this method,
    ///   otherwise no events will be delivered.
    /// - The method returns immediately after spawning the scanning task. Events are delivered
    ///   asynchronously through the registered streams.
    /// - The live phase continues indefinitely until the scanner is dropped or an error occurs.
    ///
    /// [`ScannerStatus::ReorgDetected`]: crate::types::ScannerStatus::ReorgDetected
    /// [`ScannerStatus::SwitchingToLive`]: crate::types::ScannerStatus::SwitchingToLive
    #[must_use]
    pub fn from_latest(self, count: usize) -> SyncFromLatestScannerBuilder {
        let _ = self;
        SyncFromLatestScannerBuilder::new(count)
    }

    #[must_use]
    pub fn max_block_range(mut self, max_block_range: u64) -> Self {
        self.block_range_scanner.max_block_range = max_block_range;
        self
    }

    #[must_use]
    pub fn from_block(mut self, block: impl Into<BlockNumberOrTag>) -> Self {
        self.from_block = block.into();
        self
    }

    #[must_use]
    pub fn block_confirmations(mut self, count: u64) -> Self {
        self.block_confirmations = count;
        self
    }

    /// Connects to the provider via WebSocket.
    ///
    /// Final builder method: consumes the builder and returns the built [`SyncEventScanner`].
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ws<N: Network>(self, ws_url: Url) -> TransportResult<SyncEventScanner<N>> {
        let block_range_scanner = self.block_range_scanner.connect_ws::<N>(ws_url).await?;
        Ok(SyncEventScanner { config: self, block_range_scanner, listeners: Vec::new() })
    }

    /// Connects to the provider via IPC.
    ///
    /// Final builder method: consumes the builder and returns the built [`SyncEventScanner`].
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<SyncEventScanner<N>> {
        let block_range_scanner = self.block_range_scanner.connect_ipc::<N>(ipc_path).await?;
        Ok(SyncEventScanner { config: self, block_range_scanner, listeners: Vec::new() })
    }

    /// Connects to an existing provider.
    ///
    /// Final builder method: consumes the builder and returns the built [`SyncEventScanner`].
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    #[must_use]
    pub fn connect<N: Network>(self, provider: RootProvider<N>) -> SyncEventScanner<N> {
        let block_range_scanner = self.block_range_scanner.connect::<N>(provider);
        SyncEventScanner { config: self, block_range_scanner, listeners: Vec::new() }
    }
}

impl<N: Network> SyncEventScanner<N> {
    #[must_use]
    pub fn subscribe(&mut self, filter: EventFilter) -> ReceiverStream<Message> {
        let (sender, receiver) = mpsc::channel::<Message>(MAX_BUFFERED_MESSAGES);
        self.listeners.push(EventListener { filter, sender });
        ReceiverStream::new(receiver)
    }

    /// Starts the scanner in sync (historical → live) mode.
    ///
    /// Streams from `from_block` up to the current confirmed tip using the configured
    /// `block_confirmations`, then continues streaming new confirmed ranges live.
    ///
    /// # Reorg behavior
    ///
    /// - In live mode, emits [`ScannerStatus::ReorgDetected`] and adjusts the next confirmed range
    ///   using `block_confirmations` to re-emit the confirmed portion.
    ///
    /// # Errors
    ///
    /// - `EventScannerMessage::ServiceShutdown` if the service is already shutting down.
    pub async fn start(self) -> Result<(), ScannerError> {
        let client = self.block_range_scanner.run()?;
        let stream =
            client.stream_from(self.config.from_block, self.config.block_confirmations).await?;

        let provider = self.block_range_scanner.provider().clone();
        let listeners = self.listeners.clone();

        tokio::spawn(async move {
            handle_stream(stream, &provider, &listeners, ConsumerMode::Stream).await;
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::{network::Ethereum, rpc::client::RpcClient, transports::mock::Asserter};

    #[test]
    fn test_sync_scanner_config_defaults() {
        let config = SyncScannerBuilder::new();

        assert!(matches!(config.from_block, BlockNumberOrTag::Earliest));
        assert_eq!(config.block_confirmations, DEFAULT_BLOCK_CONFIRMATIONS);
    }

    #[test]
    fn test_sync_scanner_builder_pattern() {
        let config = SyncScannerBuilder::new()
            .max_block_range(25)
            .block_confirmations(5)
            .from_block(BlockNumberOrTag::Number(50));

        assert_eq!(config.block_range_scanner.max_block_range, 25);
        assert_eq!(config.block_confirmations, 5);
        assert!(matches!(config.from_block, BlockNumberOrTag::Number(50)));
    }

    #[test]
    fn test_sync_scanner_builder_with_different_block_types() {
        let config = SyncScannerBuilder::new()
            .from_block(BlockNumberOrTag::Earliest)
            .block_confirmations(20)
            .max_block_range(100);

        assert!(matches!(config.from_block, BlockNumberOrTag::Earliest));
        assert_eq!(config.block_confirmations, 20);
        assert_eq!(config.block_range_scanner.max_block_range, 100);
    }

    #[test]
    fn test_sync_scanner_builder_with_zero_confirmations() {
        let config =
            SyncScannerBuilder::new().from_block(0).block_confirmations(0).max_block_range(75);

        assert!(matches!(config.from_block, BlockNumberOrTag::Number(0)));
        assert_eq!(config.block_confirmations, 0);
        assert_eq!(config.block_range_scanner.max_block_range, 75);
    }

    #[test]
    fn test_sync_scanner_builder_last_call_wins() {
        let config = SyncScannerBuilder::new()
            .max_block_range(25)
            .max_block_range(55)
            .max_block_range(105)
            .from_block(1)
            .from_block(2)
            .block_confirmations(5)
            .block_confirmations(7);

        assert_eq!(config.block_range_scanner.max_block_range, 105);
        assert!(matches!(config.from_block, BlockNumberOrTag::Number(2)));
        assert_eq!(config.block_confirmations, 7);
    }

    #[test]
    fn test_sync_event_stream_listeners_vector_updates() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let mut scanner = SyncScannerBuilder::new().connect::<Ethereum>(provider);
        assert_eq!(scanner.listeners.len(), 0);
        let _stream1 = scanner.subscribe(EventFilter::new());
        assert_eq!(scanner.listeners.len(), 1);
        let _stream2 = scanner.subscribe(EventFilter::new());
        let _stream3 = scanner.subscribe(EventFilter::new());
        assert_eq!(scanner.listeners.len(), 3);
    }

    #[test]
    fn test_sync_event_stream_channel_capacity() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let mut scanner = SyncScannerBuilder::new().connect::<Ethereum>(provider);
        let _stream = scanner.subscribe(EventFilter::new());
        let sender = &scanner.listeners[0].sender;
        assert_eq!(sender.capacity(), MAX_BUFFERED_MESSAGES);
    }
}
