use alloy::{
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

pub struct LiveScannerBuilder {
    block_range_scanner: BlockRangeScanner,
    block_confirmations: u64,
}

pub struct LiveEventScanner<N: Network> {
    config: LiveScannerBuilder,
    block_range_scanner: ConnectedBlockRangeScanner<N>,
    listeners: Vec<EventListener>,
}

impl LiveScannerBuilder {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            block_range_scanner: BlockRangeScanner::new(),
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

    /// Connects to the provider via WebSocket.
    ///
    /// Final builder method: consumes the builder and returns the built [`LiveEventScanner`].
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ws<N: Network>(self, ws_url: Url) -> TransportResult<LiveEventScanner<N>> {
        let block_range_scanner = self.block_range_scanner.connect_ws::<N>(ws_url).await?;
        Ok(LiveEventScanner { config: self, block_range_scanner, listeners: Vec::new() })
    }

    /// Connects to the provider via IPC.
    ///
    /// Final builder method: consumes the builder and returns the built [`LiveEventScanner`].
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<LiveEventScanner<N>> {
        let block_range_scanner = self.block_range_scanner.connect_ipc::<N>(ipc_path).await?;
        Ok(LiveEventScanner { config: self, block_range_scanner, listeners: Vec::new() })
    }

    /// Connects to an existing provider.
    ///
    /// Final builder method: consumes the builder and returns the built [`LiveEventScanner`].
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    #[must_use]
    pub fn connect<N: Network>(self, provider: RootProvider<N>) -> LiveEventScanner<N> {
        let block_range_scanner = self.block_range_scanner.connect::<N>(provider);
        LiveEventScanner { config: self, block_range_scanner, listeners: Vec::new() }
    }
}

impl<N: Network> LiveEventScanner<N> {
    #[must_use]
    pub fn subscribe(&mut self, filter: EventFilter) -> ReceiverStream<Message> {
        let (sender, receiver) = mpsc::channel::<Message>(MAX_BUFFERED_MESSAGES);
        self.listeners.push(EventListener { filter, sender });
        ReceiverStream::new(receiver)
    }

    /// Starts the scanner in live mode.
    ///
    /// Streams new blocks as they are produced, applying the configured
    /// `block_confirmations` to mitigate reorgs.
    ///
    /// # Reorg behavior
    ///
    /// - Emits [`ScannerStatus::ReorgDetected`] and adjusts the next confirmed range using
    ///   `block_confirmations` to re-emit the confirmed portion.
    ///
    /// # Errors
    ///
    /// - `EventScannerMessage::ServiceShutdown` if the service is already shutting down.
    pub async fn start(self) -> Result<(), ScannerError> {
        let client = self.block_range_scanner.run()?;
        let stream = client.stream_live(self.config.block_confirmations).await?;
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
    fn test_live_scanner_config_defaults() {
        let config = LiveScannerBuilder::new();

        assert_eq!(config.block_confirmations, DEFAULT_BLOCK_CONFIRMATIONS);
    }

    #[test]
    fn test_live_scanner_builder_pattern() {
        let config = LiveScannerBuilder::new().max_block_range(25).block_confirmations(5);

        assert_eq!(config.block_range_scanner.max_block_range, 25);
        assert_eq!(config.block_confirmations, 5);
    }

    #[test]
    fn test_live_scanner_builder_with_zero_confirmations() {
        let config = LiveScannerBuilder::new().block_confirmations(0).max_block_range(100);

        assert_eq!(config.block_confirmations, 0);
        assert_eq!(config.block_range_scanner.max_block_range, 100);
    }

    #[test]
    fn test_live_scanner_builder_last_call_wins() {
        let config = LiveScannerBuilder::new()
            .max_block_range(25)
            .max_block_range(55)
            .max_block_range(105)
            .block_confirmations(2)
            .block_confirmations(4)
            .block_confirmations(8);

        assert_eq!(config.block_range_scanner.max_block_range, 105);
        assert_eq!(config.block_confirmations, 8);
    }

    #[test]
    fn test_live_event_stream_listeners_vector_updates() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let mut scanner = LiveScannerBuilder::new().connect::<Ethereum>(provider);
        assert_eq!(scanner.listeners.len(), 0);
        let _stream1 = scanner.subscribe(EventFilter::new());
        assert_eq!(scanner.listeners.len(), 1);
        let _stream2 = scanner.subscribe(EventFilter::new());
        let _stream3 = scanner.subscribe(EventFilter::new());
        assert_eq!(scanner.listeners.len(), 3);
    }

    #[test]
    fn test_live_event_stream_channel_capacity() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let mut scanner = LiveScannerBuilder::new().connect::<Ethereum>(provider);
        let _stream = scanner.subscribe(EventFilter::new());
        let sender = &scanner.listeners[0].sender;
        assert_eq!(sender.capacity(), MAX_BUFFERED_MESSAGES);
    }
}
