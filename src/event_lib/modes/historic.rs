use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    providers::RootProvider,
    transports::{TransportResult, http::reqwest::Url},
};

use tokio_stream::wrappers::ReceiverStream;

use crate::event_lib::{
    EventScannerError,
    filter::EventFilter,
    scanner::{EventScannerMessage, EventScannerService},
};

use super::{BaseConfig, BaseConfigBuilder};

pub struct HistoricScannerConfig {
    base: BaseConfig,
    // Defaults to Earliest
    from_block: BlockNumberOrTag,
    // Defaults to Latest
    to_block: BlockNumberOrTag,
}

pub struct HistoricEventScanner<N: Network> {
    config: HistoricScannerConfig,
    inner: EventScannerService<N>,
}

impl BaseConfigBuilder for HistoricScannerConfig {
    fn base_mut(&mut self) -> &mut BaseConfig {
        &mut self.base
    }
}

impl HistoricScannerConfig {
    pub(super) fn new() -> Self {
        Self {
            base: BaseConfig::new(),
            from_block: BlockNumberOrTag::Earliest,
            to_block: BlockNumberOrTag::Latest,
        }
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
        let HistoricScannerConfig { base, from_block, to_block } = self;
        let brs = base.block_range_scanner.connect_ws::<N>(ws_url).await?;
        let config = HistoricScannerConfig { base, from_block, to_block };
        Ok(HistoricEventScanner { config, inner: EventScannerService::from_config(brs) })
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
        let HistoricScannerConfig { base, from_block, to_block } = self;
        let brs = base.block_range_scanner.connect_ipc::<N>(ipc_path).await?;
        let config = HistoricScannerConfig { base, from_block, to_block };
        Ok(HistoricEventScanner { config, inner: EventScannerService::from_config(brs) })
    }

    /// Connects to an existing provider
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub fn connect<N: Network>(
        self,
        provider: RootProvider<N>,
    ) -> TransportResult<HistoricEventScanner<N>> {
        let HistoricScannerConfig { base, from_block, to_block } = self;
        let brs = base.block_range_scanner.connect::<N>(provider)?;
        let config = HistoricScannerConfig { base, from_block, to_block };
        Ok(HistoricEventScanner { config, inner: EventScannerService::from_config(brs) })
    }
}

impl<N: Network> HistoricEventScanner<N> {
    pub fn create_event_stream(
        &mut self,
        filter: EventFilter,
    ) -> ReceiverStream<EventScannerMessage> {
        self.inner.create_event_stream(filter)
    }

    /// Calls stream historical
    ///
    /// # Errors
    ///
    /// * `EventScannerMessage::ServiceShutdown` - if the service is already shutting down.
    pub async fn run(self) -> Result<(), EventScannerError> {
        self.inner.stream_historical(self.config.from_block, self.config.to_block).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_historic_scanner_config_defaults() {
        let config = HistoricScannerConfig::new();

        assert!(matches!(config.from_block, BlockNumberOrTag::Earliest));
        assert!(matches!(config.to_block, BlockNumberOrTag::Latest));
    }

    #[test]
    fn test_historic_scanner_builder_pattern() {
        let config =
            HistoricScannerConfig::new().from_block(100u64).to_block(200u64).max_read_per_epoch(50);

        assert!(matches!(config.from_block, BlockNumberOrTag::Number(100)));
        assert!(matches!(config.to_block, BlockNumberOrTag::Number(200)));
        assert_eq!(config.base.block_range_scanner.max_read_per_epoch, 50);
    }

    #[test]
    fn test_historic_scanner_builder_pattern_chaining() {
        let config = HistoricScannerConfig::new()
            .max_read_per_epoch(25)
            .from_block(BlockNumberOrTag::Number(50))
            .to_block(BlockNumberOrTag::Number(150));

        assert_eq!(config.base.block_range_scanner.max_read_per_epoch, 25);
        assert!(matches!(config.from_block, BlockNumberOrTag::Number(50)));
        assert!(matches!(config.to_block, BlockNumberOrTag::Number(150)));
    }

    #[test]
    fn test_historic_scanner_builder_with_different_block_types() {
        let config = HistoricScannerConfig::new()
            .from_block(BlockNumberOrTag::Earliest)
            .to_block(BlockNumberOrTag::Latest);

        assert!(matches!(config.from_block, BlockNumberOrTag::Earliest));
        assert!(matches!(config.to_block, BlockNumberOrTag::Latest));
    }
}
