use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    providers::RootProvider,
    transports::{TransportResult, http::reqwest::Url},
};

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::{
    block_range_scanner::{
        BlockRangeScanner, ConnectedBlockRangeScanner, DEFAULT_BLOCK_CONFIRMATIONS,
        MAX_BUFFERED_MESSAGES,
    },
    event_scanner::{
        EventScannerError,
        filter::EventFilter,
        listener::EventListener,
        message::EventScannerMessage,
        modes::common::{ConsumerMode, handle_stream},
    },
};

pub struct LatestScannerBuilder {
    block_range_scanner: BlockRangeScanner,
    count: usize,
    from_block: BlockNumberOrTag,
    to_block: BlockNumberOrTag,
    block_confirmations: u64,
}

pub struct LatestEventScanner<N: Network> {
    config: LatestScannerBuilder,
    block_range_scanner: ConnectedBlockRangeScanner<N>,
    listeners: Vec<EventListener>,
}

impl LatestScannerBuilder {
    #[must_use]
    pub(super) fn new() -> Self {
        Self {
            block_range_scanner: BlockRangeScanner::new(),
            count: 1,
            from_block: BlockNumberOrTag::Latest,
            to_block: BlockNumberOrTag::Earliest,
            block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS,
        }
    }

    #[must_use]
    pub fn max_block_range(mut self, max_block_range: u64) -> Self {
        self.block_range_scanner.max_block_range = max_block_range;
        self
    }

    #[must_use]
    pub fn block_confirmations(mut self, confirmations: u64) -> Self {
        self.block_confirmations = confirmations;
        self
    }

    #[must_use]
    pub fn count(mut self, count: usize) -> Self {
        self.count = count;
        self
    }

    #[must_use]
    pub fn from_block(mut self, block: impl Into<BlockNumberOrTag>) -> Self {
        self.from_block = block.into();
        self
    }

    #[must_use]
    pub fn to_block(mut self, block: impl Into<BlockNumberOrTag>) -> Self {
        self.to_block = block.into();
        self
    }

    /// Connects to the provider via WebSocket.
    ///
    /// Final builder method: consumes the builder and returns the built [`LatestEventScanner`].
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ws<N: Network>(
        self,
        ws_url: Url,
    ) -> TransportResult<LatestEventScanner<N>> {
        let block_range_scanner = self.block_range_scanner.connect_ws::<N>(ws_url).await?;
        Ok(LatestEventScanner { config: self, block_range_scanner, listeners: Vec::new() })
    }

    /// Connects to the provider via IPC.
    ///
    /// Final builder method: consumes the builder and returns the built [`LatestEventScanner`].
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<LatestEventScanner<N>> {
        let block_range_scanner = self.block_range_scanner.connect_ipc::<N>(ipc_path).await?;
        Ok(LatestEventScanner { config: self, block_range_scanner, listeners: Vec::new() })
    }

    /// Connects to an existing provider.
    ///
    /// Final builder method: consumes the builder and returns the built [`LatestEventScanner`].
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    #[must_use]
    pub fn connect<N: Network>(self, provider: RootProvider<N>) -> LatestEventScanner<N> {
        let block_range_scanner = self.block_range_scanner.connect::<N>(provider);
        LatestEventScanner { config: self, block_range_scanner, listeners: Vec::new() }
    }
}

impl<N: Network> LatestEventScanner<N> {
    #[must_use]
    pub fn subscribe(&mut self, filter: EventFilter) -> ReceiverStream<EventScannerMessage> {
        let (sender, receiver) = mpsc::channel::<EventScannerMessage>(MAX_BUFFERED_MESSAGES);
        self.listeners.push(EventListener { filter, sender });
        ReceiverStream::new(receiver)
    }

    /// Scans a block range and collects the latest `count` matching events per registered listener.
    ///
    /// Emits a single message per listener with up to `count` logs, ordered oldest→newest.
    ///
    /// # Reorg behavior
    ///
    /// Performs a reverse-ordered rewind over the range, periodically checking the tip hash. If a
    /// reorg is detected, emits [`ScannerStatus::ReorgDetected`], resets the rewind start to the
    /// updated tip, and resumes until completion. Final log delivery preserves chronological order.
    ///
    /// # Errors
    ///
    /// - Returns `EventScannerError` if the scanner fails to start or fetching logs fails.
    ///
    /// [`ScannerStatus::ReorgDetected`]: crate::types::ScannerStatus::ReorgDetected
    pub async fn start(self) -> Result<(), EventScannerError> {
        let client = self.block_range_scanner.run()?;
        let stream = client.rewind(self.config.from_block, self.config.to_block).await?;
        handle_stream(
            stream,
            self.block_range_scanner.provider(),
            &self.listeners,
            ConsumerMode::CollectLatest { count: self.config.count },
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
    fn test_latest_scanner_config_defaults() {
        let config = LatestScannerBuilder::new();

        assert_eq!(config.count, 1);
        assert!(matches!(config.from_block, BlockNumberOrTag::Latest));
        assert!(matches!(config.to_block, BlockNumberOrTag::Earliest));
        assert_eq!(config.block_confirmations, DEFAULT_BLOCK_CONFIRMATIONS);
    }

    #[test]
    fn test_latest_scanner_builder_pattern_random_order_chaining() {
        let config = LatestScannerBuilder::new()
            .max_block_range(25)
            .block_confirmations(5)
            .count(3)
            .from_block(BlockNumberOrTag::Number(50))
            .to_block(BlockNumberOrTag::Number(150));

        assert_eq!(config.block_range_scanner.max_block_range, 25);
        assert_eq!(config.block_confirmations, 5);
        assert_eq!(config.count, 3);
        assert!(matches!(config.from_block, BlockNumberOrTag::Number(50)));
        assert!(matches!(config.to_block, BlockNumberOrTag::Number(150)));
    }

    #[test]
    fn test_latest_scanner_builder_with_different_block_types() {
        let config = LatestScannerBuilder::new()
            .from_block(BlockNumberOrTag::Earliest)
            .to_block(BlockNumberOrTag::Latest)
            .count(10)
            .block_confirmations(20);

        assert!(matches!(config.from_block, BlockNumberOrTag::Earliest));
        assert!(matches!(config.to_block, BlockNumberOrTag::Latest));
        assert_eq!(config.count, 10);
        assert_eq!(config.block_confirmations, 20);
    }

    #[test]
    fn test_latest_scanner_builder_last_call_wins() {
        let config = LatestScannerBuilder::new()
            .count(1)
            .count(2)
            .count(3)
            .from_block(10)
            .from_block(20)
            .to_block(100)
            .to_block(200)
            .block_confirmations(5)
            .block_confirmations(7)
            .max_block_range(50)
            .max_block_range(60);

        assert_eq!(config.count, 3);
        assert!(matches!(config.from_block, BlockNumberOrTag::Number(20)));
        assert!(matches!(config.to_block, BlockNumberOrTag::Number(200)));
        assert_eq!(config.block_confirmations, 7);
        assert_eq!(config.block_range_scanner.max_block_range, 60);
    }

    #[test]
    fn test_latest_event_stream_listeners_vector_updates() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let mut scanner = LatestScannerBuilder::new().connect::<Ethereum>(provider);
        assert_eq!(scanner.listeners.len(), 0);
        let _stream1 = scanner.subscribe(EventFilter::new());
        assert_eq!(scanner.listeners.len(), 1);
        let _stream2 = scanner.subscribe(EventFilter::new());
        let _stream3 = scanner.subscribe(EventFilter::new());
        assert_eq!(scanner.listeners.len(), 3);
    }

    #[test]
    fn test_latest_event_stream_channel_capacity() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let mut scanner = LatestScannerBuilder::new().connect::<Ethereum>(provider);
        let _stream = scanner.subscribe(EventFilter::new());
        let sender = &scanner.listeners[0].sender;
        assert_eq!(sender.capacity(), MAX_BUFFERED_MESSAGES);
    }
}
