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

pub struct SyncScannerBuilder<N: Network> {
    block_range_scanner: BlockRangeScanner<N>,
    from_block: BlockNumberOrTag,
    block_confirmations: u64,
}

pub struct SyncEventScanner<N: Network> {
    config: SyncScannerBuilder<N>,
    block_range_scanner: ConnectedBlockRangeScanner<N>,
    listeners: Vec<EventListener>,
}

impl<N: Network> SyncScannerBuilder<N> {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            block_range_scanner: BlockRangeScanner::new(),
            from_block: BlockNumberOrTag::Earliest,
            block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS,
        }
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

    /// Adds a fallback provider (can add multiple)
    ///
    /// # Errors
    ///
    /// Will panic if the provider does not implement pubsub
    #[must_use]
    pub fn fallback_provider(mut self, provider: RootProvider<N>) -> Self {
        self.block_range_scanner.fallback_providers.push(provider);
        self
    }

    /// Connects to the provider via WebSocket.
    ///
    /// Final builder method: consumes the builder and returns the built [`SyncEventScanner`].
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ws(self, ws_url: Url) -> TransportResult<SyncEventScanner<N>> {
        let block_range_scanner = self.block_range_scanner.clone().connect_ws(ws_url).await?;
        Ok(SyncEventScanner { config: self, block_range_scanner, listeners: Vec::new() })
    }

    /// Connects to the provider via IPC.
    ///
    /// Final builder method: consumes the builder and returns the built [`SyncEventScanner`].
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ipc(self, ipc_path: String) -> TransportResult<SyncEventScanner<N>> {
        let block_range_scanner = self.block_range_scanner.clone().connect_ipc(ipc_path).await?;
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
    pub fn connect(self, provider: RootProvider<N>) -> SyncEventScanner<N> {
        let block_range_scanner = self.block_range_scanner.clone().connect(provider);
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
        handle_stream(
            stream,
            self.block_range_scanner.provider(),
            &self.listeners,
            ConsumerMode::Stream,
        )
        .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::{network::Ethereum, rpc::client::RpcClient, transports::mock::Asserter};

    #[test]
    fn test_sync_scanner_config_defaults() {
        let config: SyncScannerBuilder<Ethereum> = SyncScannerBuilder::new();

        assert!(matches!(config.from_block, BlockNumberOrTag::Earliest));
        assert_eq!(config.block_confirmations, DEFAULT_BLOCK_CONFIRMATIONS);
    }

    #[test]
    fn test_sync_scanner_builder_pattern() {
        let config: SyncScannerBuilder<Ethereum> = SyncScannerBuilder::new()
            .max_block_range(25)
            .block_confirmations(5)
            .from_block(BlockNumberOrTag::Number(50));

        assert_eq!(config.block_range_scanner.max_block_range, 25);
        assert_eq!(config.block_confirmations, 5);
        assert!(matches!(config.from_block, BlockNumberOrTag::Number(50)));
    }

    #[test]
    fn test_sync_scanner_builder_with_different_block_types() {
        let config: SyncScannerBuilder<Ethereum> = SyncScannerBuilder::new()
            .from_block(BlockNumberOrTag::Earliest)
            .block_confirmations(20)
            .max_block_range(100);

        assert!(matches!(config.from_block, BlockNumberOrTag::Earliest));
        assert_eq!(config.block_confirmations, 20);
        assert_eq!(config.block_range_scanner.max_block_range, 100);
    }

    #[test]
    fn test_sync_scanner_builder_with_zero_confirmations() {
        let config: SyncScannerBuilder<Ethereum> =
            SyncScannerBuilder::new().from_block(0).block_confirmations(0).max_block_range(75);

        assert!(matches!(config.from_block, BlockNumberOrTag::Number(0)));
        assert_eq!(config.block_confirmations, 0);
        assert_eq!(config.block_range_scanner.max_block_range, 75);
    }

    #[test]
    fn test_sync_scanner_builder_last_call_wins() {
        let config: SyncScannerBuilder<Ethereum> = SyncScannerBuilder::new()
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
        let mut scanner = SyncScannerBuilder::new().connect(provider);
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
        let mut scanner = SyncScannerBuilder::new().connect(provider);
        let _stream = scanner.subscribe(EventFilter::new());
        let sender = &scanner.listeners[0].sender;
        assert_eq!(sender.capacity(), MAX_BUFFERED_MESSAGES);
    }
}
