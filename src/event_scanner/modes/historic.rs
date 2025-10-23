use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    providers::RootProvider,
    transports::{TransportResult, http::reqwest::Url},
};

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::{
    block_range_scanner::{BlockRangeScanner, ConnectedBlockRangeScanner, MAX_BUFFERED_MESSAGES},
    event_scanner::{
        EventScannerError,
        filter::EventFilter,
        listener::EventListener,
        message::EventScannerMessage,
        modes::common::{ConsumerMode, handle_stream},
    },
};

pub struct HistoricScannerBuilder {
    block_range_scanner: BlockRangeScanner,
    from_block: BlockNumberOrTag,
    to_block: BlockNumberOrTag,
}

pub struct HistoricEventScanner<N: Network> {
    config: HistoricScannerBuilder,
    block_range_scanner: ConnectedBlockRangeScanner<N>,
    listeners: Vec<EventListener>,
}

impl HistoricScannerBuilder {
    #[must_use]
    pub(super) fn new() -> Self {
        Self {
            block_range_scanner: BlockRangeScanner::new(),
            from_block: BlockNumberOrTag::Earliest,
            to_block: BlockNumberOrTag::Latest,
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
    pub fn to_block(mut self, block: impl Into<BlockNumberOrTag>) -> Self {
        self.to_block = block.into();
        self
    }

    /// Connects to the provider via WebSocket.
    ///
    /// Final builder method: consumes the builder and returns the built [`HistoricEventScanner`].
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ws<N: Network>(
        self,
        ws_url: Url,
    ) -> TransportResult<HistoricEventScanner<N>> {
        let block_range_scanner = self.block_range_scanner.connect_ws::<N>(ws_url).await?;
        Ok(HistoricEventScanner { config: self, block_range_scanner, listeners: Vec::new() })
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
    ) -> TransportResult<HistoricEventScanner<N>> {
        let block_range_scanner = self.block_range_scanner.connect_ipc::<N>(ipc_path).await?;
        Ok(HistoricEventScanner { config: self, block_range_scanner, listeners: Vec::new() })
    }

    /// Connects to an existing provider.
    ///
    /// Final builder method: consumes the builder and returns the built [`HistoricEventScanner`].
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    #[must_use]
    pub fn connect<N: Network>(self, provider: RootProvider<N>) -> HistoricEventScanner<N> {
        let block_range_scanner = self.block_range_scanner.connect::<N>(provider);
        HistoricEventScanner { config: self, block_range_scanner, listeners: Vec::new() }
    }
}

impl<N: Network> HistoricEventScanner<N> {
    #[must_use]
    pub fn subscribe(&mut self, filter: EventFilter) -> ReceiverStream<EventScannerMessage> {
        let (sender, receiver) = mpsc::channel::<EventScannerMessage>(MAX_BUFFERED_MESSAGES);
        self.listeners.push(EventListener { filter, sender });
        ReceiverStream::new(receiver)
    }

    /// Starts the scanner in historical mode.
    ///
    /// Scans from `from_block` to `to_block` (inclusive), emitting block ranges
    /// and matching logs to registered listeners.
    ///
    /// # Errors
    ///
    /// - `EventScannerMessage::ServiceShutdown` if the service is already shutting down.
    pub async fn start(self) -> Result<(), EventScannerError> {
        let client = self.block_range_scanner.run()?;
        let stream = client.stream_historical(self.config.from_block, self.config.to_block).await?;
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
    fn test_historic_scanner_config_defaults() {
        let config = HistoricScannerBuilder::new();

        assert!(matches!(config.from_block, BlockNumberOrTag::Earliest));
        assert!(matches!(config.to_block, BlockNumberOrTag::Latest));
    }

    #[test]
    fn test_historic_scanner_builder_pattern_random_order_chaining() {
        let config =
            HistoricScannerBuilder::new().to_block(200).max_block_range(50).from_block(100);

        assert!(matches!(config.from_block, BlockNumberOrTag::Number(100)));
        assert!(matches!(config.to_block, BlockNumberOrTag::Number(200)));
        assert_eq!(config.block_range_scanner.max_block_range, 50);
    }

    #[test]
    fn test_historic_scanner_builder_with_different_block_types() {
        let config = HistoricScannerBuilder::new()
            .from_block(BlockNumberOrTag::Earliest)
            .to_block(BlockNumberOrTag::Latest);

        assert!(matches!(config.from_block, BlockNumberOrTag::Earliest));
        assert!(matches!(config.to_block, BlockNumberOrTag::Latest));
    }

    #[test]
    fn test_historic_scanner_builder_last_call_wins() {
        let config = HistoricScannerBuilder::new()
            .max_block_range(25)
            .max_block_range(55)
            .max_block_range(105)
            .from_block(1)
            .from_block(2)
            .to_block(100)
            .to_block(200);

        assert_eq!(config.block_range_scanner.max_block_range, 105);
        assert!(matches!(config.from_block, BlockNumberOrTag::Number(2)));
        assert!(matches!(config.to_block, BlockNumberOrTag::Number(200)));
    }

    #[test]
    fn test_historic_event_stream_listeners_vector_updates() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let mut scanner = HistoricScannerBuilder::new().connect::<Ethereum>(provider);
        assert_eq!(scanner.listeners.len(), 0);
        let _stream1 = scanner.subscribe(EventFilter::new());
        assert_eq!(scanner.listeners.len(), 1);
        let _stream2 = scanner.subscribe(EventFilter::new());
        let _stream3 = scanner.subscribe(EventFilter::new());
        assert_eq!(scanner.listeners.len(), 3);
    }

    #[test]
    fn test_historic_event_stream_channel_capacity() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let mut scanner = HistoricScannerBuilder::new().connect::<Ethereum>(provider);
        let _stream = scanner.subscribe(EventFilter::new());
        let sender = &scanner.listeners[0].sender;
        assert_eq!(sender.capacity(), MAX_BUFFERED_MESSAGES);
    }
}
