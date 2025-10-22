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
        ConnectedBlockRangeScanner, DEFAULT_BLOCK_CONFIRMATIONS, MAX_BUFFERED_MESSAGES,
    },
    event_scanner::{
        EventScannerError,
        consumer::{ConsumerMode, handle_stream},
        filter::EventFilter,
        listener::EventListener,
        message::EventScannerMessage,
    },
};

use super::{BaseConfig, BaseConfigBuilder};

pub struct LatestScannerBuilder {
    base: BaseConfig,
    // Defatuls to 1
    count: usize,
    // Defaults to Latest
    from_block: BlockNumberOrTag,
    // Defaults to Earliest
    to_block: BlockNumberOrTag,
    // Defaults to 0
    block_confirmations: u64,
}

pub struct LatestEventScanner<N: Network> {
    config: LatestScannerBuilder,
    block_range_scanner: ConnectedBlockRangeScanner<N>,
    listeners: Vec<EventListener>,
}

impl BaseConfigBuilder for LatestScannerBuilder {
    fn base_mut(&mut self) -> &mut BaseConfig {
        &mut self.base
    }
}

impl LatestScannerBuilder {
    pub(super) fn new() -> Self {
        Self {
            base: BaseConfig::new(),
            count: 1,
            from_block: BlockNumberOrTag::Latest,
            to_block: BlockNumberOrTag::Earliest,
            block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS,
        }
    }

    #[must_use]
    pub fn block_confirmations(mut self, count: u64) -> Self {
        self.block_confirmations = count;
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

    /// Connects to the provider via WebSocket
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ws<N: Network>(
        self,
        ws_url: Url,
    ) -> TransportResult<LatestEventScanner<N>> {
        let block_range_scanner = self.base.block_range_scanner.connect_ws::<N>(ws_url).await?;
        Ok(LatestEventScanner { config: self, block_range_scanner, listeners: Vec::new() })
    }

    /// Connects to the provider via IPC
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<LatestEventScanner<N>> {
        let block_range_scanner = self.base.block_range_scanner.connect_ipc::<N>(ipc_path).await?;
        Ok(LatestEventScanner { config: self, block_range_scanner, listeners: Vec::new() })
    }

    /// Connects to an existing provider
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    #[must_use]
    pub fn connect<N: Network>(self, provider: RootProvider<N>) -> LatestEventScanner<N> {
        let block_range_scanner = self.base.block_range_scanner.connect::<N>(provider);
        LatestEventScanner { config: self, block_range_scanner, listeners: Vec::new() }
    }
}

impl<N: Network> LatestEventScanner<N> {
    pub fn create_event_stream(
        &mut self,
        filter: EventFilter,
    ) -> ReceiverStream<EventScannerMessage> {
        let (sender, receiver) = mpsc::channel::<EventScannerMessage>(MAX_BUFFERED_MESSAGES);
        self.listeners.push(EventListener { filter, sender });
        ReceiverStream::new(receiver)
    }

    /// Calls stream latest
    ///
    /// # Errors
    ///
    /// * `EventScannerMessage::ServiceShutdown` - if the service is already shutting down.
    #[allow(clippy::unused_async)]
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

    #[test]
    fn test_latest_scanner_config_defaults() {
        let config = LatestScannerBuilder::new();

        assert_eq!(config.count, 1);
        assert!(matches!(config.from_block, BlockNumberOrTag::Latest));
        assert!(matches!(config.to_block, BlockNumberOrTag::Earliest));
        assert_eq!(config.block_confirmations, DEFAULT_BLOCK_CONFIRMATIONS);
    }

    #[test]
    fn test_latest_scanner_builder_pattern() {
        let config = LatestScannerBuilder::new()
            .count(5)
            .from_block(100)
            .to_block(200)
            .block_confirmations(10)
            .max_block_range(50);

        assert_eq!(config.count, 5);
        assert!(matches!(config.from_block, BlockNumberOrTag::Number(100)));
        assert!(matches!(config.to_block, BlockNumberOrTag::Number(200)));
        assert_eq!(config.block_confirmations, 10);
        assert_eq!(config.base.block_range_scanner.max_block_range, 50);
    }

    #[test]
    fn test_latest_scanner_builder_pattern_chaining() {
        let config = LatestScannerBuilder::new()
            .max_block_range(25)
            .block_confirmations(5)
            .count(3)
            .from_block(BlockNumberOrTag::Number(50))
            .to_block(BlockNumberOrTag::Number(150));

        assert_eq!(config.base.block_range_scanner.max_block_range, 25);
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
}
