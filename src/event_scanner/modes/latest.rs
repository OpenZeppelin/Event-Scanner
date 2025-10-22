use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    providers::RootProvider,
    transports::{TransportResult, http::reqwest::Url},
};

use tokio_stream::wrappers::ReceiverStream;

use crate::{
    block_range_scanner::DEFAULT_BLOCK_CONFIRMATIONS,
    event_scanner::{
        EventScannerError,
        filter::EventFilter,
        message::EventScannerMessage,
        scanner::EventScannerService,
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
    // Defaults to false
    switch_to_live: bool,
}

pub struct LatestEventScanner<N: Network> {
    #[allow(dead_code)]
    config: LatestScannerBuilder,
    inner: EventScannerService<N>,
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
            switch_to_live: false,
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

    #[must_use]
    pub fn then_live(mut self) -> Self {
        self.switch_to_live = true;
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
        let brs = self.base.block_range_scanner.connect_ws::<N>(ws_url).await?;
        Ok(LatestEventScanner { config: self, inner: EventScannerService::from_config(brs) })
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
        let brs = self.base.block_range_scanner.connect_ipc::<N>(ipc_path).await?;
        Ok(LatestEventScanner { config: self, inner: EventScannerService::from_config(brs) })
    }

    /// Connects to an existing provider
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    #[must_use]
    pub fn connect<N: Network>(self, provider: RootProvider<N>) -> LatestEventScanner<N> {
        let brs = self.base.block_range_scanner.connect::<N>(provider);
        LatestEventScanner { config: self, inner: EventScannerService::from_config(brs) }
    }
}

impl<N: Network> LatestEventScanner<N> {
    pub fn create_event_stream(
        &mut self,
        filter: EventFilter,
    ) -> ReceiverStream<EventScannerMessage> {
        self.inner.create_event_stream(filter)
    }

    /// Calls stream latest
    ///
    /// # Errors
    ///
    /// * `EventScannerMessage::ServiceShutdown` - if the service is already shutting down.
    #[allow(clippy::unused_async)]
    pub async fn start(self) -> Result<(), EventScannerError> {
        self.inner
            .stream_latest(self.config.count, self.config.from_block, self.config.to_block)
            .await
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
        assert!(!config.switch_to_live);
    }

    #[test]
    fn test_latest_scanner_builder_pattern() {
        let config = LatestScannerBuilder::new()
            .count(5)
            .from_block(100)
            .to_block(200)
            .block_confirmations(10)
            .then_live()
            .max_block_range(50);

        assert_eq!(config.count, 5);
        assert!(matches!(config.from_block, BlockNumberOrTag::Number(100)));
        assert!(matches!(config.to_block, BlockNumberOrTag::Number(200)));
        assert_eq!(config.block_confirmations, 10);
        assert!(config.switch_to_live);
        assert_eq!(config.base.block_range_scanner.max_block_range, 50);
    }

    #[test]
    fn test_latest_scanner_builder_pattern_chaining() {
        let config = LatestScannerBuilder::new()
            .max_block_range(25)
            .block_confirmations(5)
            .count(3)
            .from_block(BlockNumberOrTag::Number(50))
            .to_block(BlockNumberOrTag::Number(150))
            .then_live();

        assert_eq!(config.base.block_range_scanner.max_block_range, 25);
        assert_eq!(config.block_confirmations, 5);
        assert_eq!(config.count, 3);
        assert!(matches!(config.from_block, BlockNumberOrTag::Number(50)));
        assert!(matches!(config.to_block, BlockNumberOrTag::Number(150)));
        assert!(config.switch_to_live);
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
        assert!(!config.switch_to_live);
    }

    #[test]
    fn test_latest_scanner_then_live_method() {
        let config = LatestScannerBuilder::new().then_live();
        assert!(config.switch_to_live);

        let config_without_live = LatestScannerBuilder::new();
        assert!(!config_without_live.switch_to_live);
    }
}
