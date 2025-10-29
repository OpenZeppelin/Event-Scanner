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

pub struct SyncFromBlockEventScannerBuilder {
    block_range_scanner: BlockRangeScanner,
    from_block: BlockNumberOrTag,
    block_confirmations: u64,
}

pub struct SyncFromBlockEventScanner<N: Network> {
    config: SyncFromBlockEventScannerBuilder,
    block_range_scanner: ConnectedBlockRangeScanner<N>,
    listeners: Vec<EventListener>,
}

impl SyncFromBlockEventScannerBuilder {
    #[must_use]
    pub(crate) fn new(from_block: impl Into<BlockNumberOrTag>) -> Self {
        Self {
            block_range_scanner: BlockRangeScanner::new(),
            from_block: from_block.into(),
            block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS,
        }
    }

    #[must_use]
    pub fn max_block_range(mut self, max_block_range: u64) -> Self {
        self.block_range_scanner.max_block_range = max_block_range;
        self
    }

    #[must_use]
    pub fn block_confirmations(mut self, count: u64) -> Self {
        self.block_confirmations = count;
        self
    }

    /// Connects to the provider via WebSocket.
    ///
    /// Final builder method: consumes the builder and returns the built
    /// [`SyncFromBlockEventScanner`].
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ws<N: Network>(
        self,
        ws_url: Url,
    ) -> TransportResult<SyncFromBlockEventScanner<N>> {
        let block_range_scanner = self.block_range_scanner.connect_ws::<N>(ws_url).await?;
        Ok(SyncFromBlockEventScanner { config: self, block_range_scanner, listeners: Vec::new() })
    }

    /// Connects to the provider via IPC.
    ///
    /// Final builder method: consumes the builder and returns the built
    /// [`SyncFromBlockEventScanner`].
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<SyncFromBlockEventScanner<N>> {
        let block_range_scanner = self.block_range_scanner.connect_ipc::<N>(ipc_path).await?;
        Ok(SyncFromBlockEventScanner { config: self, block_range_scanner, listeners: Vec::new() })
    }

    /// Connects to an existing provider.
    ///
    /// Final builder method: consumes the builder and returns the built
    /// [`SyncFromBlockEventScanner`].
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    #[must_use]
    pub fn connect<N: Network>(self, provider: RootProvider<N>) -> SyncFromBlockEventScanner<N> {
        let block_range_scanner = self.block_range_scanner.connect::<N>(provider);
        SyncFromBlockEventScanner { config: self, block_range_scanner, listeners: Vec::new() }
    }
}

impl<N: Network> SyncFromBlockEventScanner<N> {
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
    fn sync_scanner_config_defaults() {
        let config = SyncFromBlockEventScannerBuilder::new(BlockNumberOrTag::Earliest);

        assert!(matches!(config.from_block, BlockNumberOrTag::Earliest));
        assert_eq!(config.block_confirmations, DEFAULT_BLOCK_CONFIRMATIONS);
    }

    #[test]
    fn sync_scanner_builder_pattern() {
        let config =
            SyncFromBlockEventScannerBuilder::new(50).max_block_range(25).block_confirmations(5);

        assert_eq!(config.block_range_scanner.max_block_range, 25);
        assert_eq!(config.block_confirmations, 5);
        assert!(matches!(config.from_block, BlockNumberOrTag::Number(50)));
    }

    #[test]
    fn sync_scanner_builder_with_different_block_types() {
        let config = SyncFromBlockEventScannerBuilder::new(BlockNumberOrTag::Earliest)
            .block_confirmations(20)
            .max_block_range(100);

        assert!(matches!(config.from_block, BlockNumberOrTag::Earliest));
        assert_eq!(config.block_confirmations, 20);
        assert_eq!(config.block_range_scanner.max_block_range, 100);
    }

    #[test]
    fn sync_scanner_builder_with_zero_confirmations() {
        let config =
            SyncFromBlockEventScannerBuilder::new(0).block_confirmations(0).max_block_range(75);

        assert!(matches!(config.from_block, BlockNumberOrTag::Number(0)));
        assert_eq!(config.block_confirmations, 0);
        assert_eq!(config.block_range_scanner.max_block_range, 75);
    }

    #[test]
    fn sync_scanner_builder_last_call_wins() {
        let config = SyncFromBlockEventScannerBuilder::new(2)
            .max_block_range(25)
            .max_block_range(55)
            .max_block_range(105)
            .block_confirmations(5)
            .block_confirmations(7);

        assert_eq!(config.block_range_scanner.max_block_range, 105);
        assert!(matches!(config.from_block, BlockNumberOrTag::Number(2)));
        assert_eq!(config.block_confirmations, 7);
    }

    #[test]
    fn sync_event_stream_listeners_vector_updates() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let mut scanner = SyncFromBlockEventScannerBuilder::new(BlockNumberOrTag::Earliest)
            .connect::<Ethereum>(provider);
        assert_eq!(scanner.listeners.len(), 0);
        let _stream1 = scanner.subscribe(EventFilter::new());
        assert_eq!(scanner.listeners.len(), 1);
        let _stream2 = scanner.subscribe(EventFilter::new());
        let _stream3 = scanner.subscribe(EventFilter::new());
        assert_eq!(scanner.listeners.len(), 3);
    }

    #[test]
    fn sync_event_stream_channel_capacity() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let mut scanner = SyncFromBlockEventScannerBuilder::new(BlockNumberOrTag::Earliest)
            .connect::<Ethereum>(provider);
        let _stream = scanner.subscribe(EventFilter::new());
        let sender = &scanner.listeners[0].sender;
        assert_eq!(sender.capacity(), MAX_BUFFERED_MESSAGES);
    }
}
