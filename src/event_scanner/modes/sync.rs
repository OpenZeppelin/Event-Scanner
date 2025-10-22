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
        scanner::{EventScannerMessage, EventScannerService},
    },
};

use super::{BaseConfig, BaseConfigBuilder};

pub struct SyncScannerConfig {
    base: BaseConfig,
    // Defaults to Earliest
    from_block: BlockNumberOrTag,
    // Defaults to 0
    block_confirmations: u64,
}

pub struct SyncEventScanner<N: Network> {
    config: SyncScannerConfig,
    inner: EventScannerService<N>,
}

impl BaseConfigBuilder for SyncScannerConfig {
    fn base_mut(&mut self) -> &mut BaseConfig {
        &mut self.base
    }
}

impl SyncScannerConfig {
    pub(super) fn new() -> Self {
        Self {
            base: BaseConfig::new(),
            from_block: BlockNumberOrTag::Earliest,
            block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS,
        }
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

    /// Connects to the provider via WebSocket
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ws<N: Network>(self, ws_url: Url) -> TransportResult<SyncEventScanner<N>> {
        let SyncScannerConfig { base, from_block, block_confirmations } = self;
        let brs = base.block_range_scanner.connect_ws::<N>(ws_url).await?;
        let config = SyncScannerConfig { base, from_block, block_confirmations };
        Ok(SyncEventScanner { config, inner: EventScannerService::from_config(brs) })
    }

    /// Connects to the provider via IPC
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<SyncEventScanner<N>> {
        let SyncScannerConfig { base, from_block, block_confirmations } = self;
        let brs = base.block_range_scanner.connect_ipc::<N>(ipc_path).await?;
        let config = SyncScannerConfig { base, from_block, block_confirmations };
        Ok(SyncEventScanner { config, inner: EventScannerService::from_config(brs) })
    }

    /// Connects to an existing provider
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub fn connect<N: Network>(
        self,
        provider: RootProvider<N>,
    ) -> TransportResult<SyncEventScanner<N>> {
        let SyncScannerConfig { base, from_block, block_confirmations } = self;
        let brs = base.block_range_scanner.connect::<N>(provider);
        let config = SyncScannerConfig { base, from_block, block_confirmations };
        Ok(SyncEventScanner { config, inner: EventScannerService::from_config(brs) })
    }
}

impl<N: Network> SyncEventScanner<N> {
    pub fn create_event_stream(
        &mut self,
        filter: EventFilter,
    ) -> ReceiverStream<EventScannerMessage> {
        self.inner.create_event_stream(filter)
    }

    /// Calls stream from
    ///
    /// # Errors
    ///
    /// * `EventScannerMessage::ServiceShutdown` - if the service is already shutting down.
    pub async fn start(self) -> Result<(), EventScannerError> {
        self.inner.stream_from(self.config.from_block, self.config.block_confirmations).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_scanner_config_defaults() {
        let config = SyncScannerConfig::new();

        assert!(matches!(config.from_block, BlockNumberOrTag::Earliest));
        assert_eq!(config.block_confirmations, DEFAULT_BLOCK_CONFIRMATIONS);
    }

    #[test]
    fn test_sync_scanner_builder_pattern() {
        let config =
            SyncScannerConfig::new().from_block(100).block_confirmations(10).max_block_range(50);

        assert!(matches!(config.from_block, BlockNumberOrTag::Number(100)));
        assert_eq!(config.block_confirmations, 10);
        assert_eq!(config.base.block_range_scanner.max_block_range, 50);
    }

    #[test]
    fn test_sync_scanner_builder_pattern_chaining() {
        let config = SyncScannerConfig::new()
            .max_block_range(25)
            .block_confirmations(5)
            .from_block(BlockNumberOrTag::Number(50));

        assert_eq!(config.base.block_range_scanner.max_block_range, 25);
        assert_eq!(config.block_confirmations, 5);
        assert!(matches!(config.from_block, BlockNumberOrTag::Number(50)));
    }

    #[test]
    fn test_sync_scanner_builder_with_different_block_types() {
        let config = SyncScannerConfig::new()
            .from_block(BlockNumberOrTag::Earliest)
            .block_confirmations(20)
            .max_block_range(100);

        assert!(matches!(config.from_block, BlockNumberOrTag::Earliest));
        assert_eq!(config.block_confirmations, 20);
        assert_eq!(config.base.block_range_scanner.max_block_range, 100);
    }

    #[test]
    fn test_sync_scanner_builder_with_zero_confirmations() {
        let config =
            SyncScannerConfig::new().from_block(0).block_confirmations(0).max_block_range(75);

        assert!(matches!(config.from_block, BlockNumberOrTag::Number(0)));
        assert_eq!(config.block_confirmations, 0);
        assert_eq!(config.base.block_range_scanner.max_block_range, 75);
    }
}
