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
    // Defaults to Earliest
    from_block: BlockNumberOrTag,
    // Defaults to Latest
    to_block: BlockNumberOrTag,
}

pub struct HistoricEventScanner<N: Network> {
    config: HistoricScannerBuilder,
    block_range_scanner: ConnectedBlockRangeScanner<N>,
    listeners: Vec<EventListener>,
}

impl HistoricScannerBuilder {
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

    /// Connects to the provider via WebSocket
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

    /// Connects to the provider via IPC
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

    /// Connects to an existing provider
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
    pub fn create_event_stream(
        &mut self,
        filter: EventFilter,
    ) -> ReceiverStream<EventScannerMessage> {
        let (sender, receiver) = mpsc::channel::<EventScannerMessage>(MAX_BUFFERED_MESSAGES);
        self.listeners.push(EventListener { filter, sender });
        ReceiverStream::new(receiver)
    }

    /// Calls stream historical
    ///
    /// # Errors
    ///
    /// * `EventScannerMessage::ServiceShutdown` - if the service is already shutting down.
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

    #[test]
    fn test_historic_scanner_config_defaults() {
        let config = HistoricScannerBuilder::new();

        assert!(matches!(config.from_block, BlockNumberOrTag::Earliest));
        assert!(matches!(config.to_block, BlockNumberOrTag::Latest));
    }

    #[test]
    fn test_historic_scanner_builder_pattern() {
        let config =
            HistoricScannerBuilder::new().from_block(100u64).to_block(200u64).max_block_range(50);

        assert!(matches!(config.from_block, BlockNumberOrTag::Number(100)));
        assert!(matches!(config.to_block, BlockNumberOrTag::Number(200)));
        assert_eq!(config.block_range_scanner.max_block_range, 50);
    }

    #[test]
    fn test_historic_scanner_builder_pattern_chaining() {
        let config = HistoricScannerBuilder::new()
            .max_block_range(25)
            .from_block(BlockNumberOrTag::Number(50))
            .to_block(BlockNumberOrTag::Number(150));

        assert_eq!(config.block_range_scanner.max_block_range, 25);
        assert!(matches!(config.from_block, BlockNumberOrTag::Number(50)));
        assert!(matches!(config.to_block, BlockNumberOrTag::Number(150)));
    }

    #[test]
    fn test_historic_scanner_builder_with_different_block_types() {
        let config = HistoricScannerBuilder::new()
            .from_block(BlockNumberOrTag::Earliest)
            .to_block(BlockNumberOrTag::Latest);

        assert!(matches!(config.from_block, BlockNumberOrTag::Earliest));
        assert!(matches!(config.to_block, BlockNumberOrTag::Latest));
    }
}
