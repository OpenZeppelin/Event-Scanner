use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    providers::RootProvider,
    transports::{TransportResult, http::reqwest::Url},
};

use tokio_stream::wrappers::ReceiverStream;

use crate::event_lib::{
    filter::EventFilter,
    scanner::{EventScannerError, EventScannerMessage, EventScannerService},
};

use super::{BaseConfig, BaseConfigBuilder};

pub struct HistoricModeConfig {
    base: BaseConfig,
    from_block: BlockNumberOrTag,
    to_block: BlockNumberOrTag,
}

pub struct HistoricModeScanner<N: Network> {
    config: HistoricModeConfig,
    inner: EventScannerService<N>,
}

impl BaseConfigBuilder for HistoricModeConfig {
    fn base_mut(&mut self) -> &mut BaseConfig {
        &mut self.base
    }
}

impl HistoricModeConfig {
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
    ) -> TransportResult<HistoricModeScanner<N>> {
        let HistoricModeConfig { base, from_block, to_block } = self;
        let brs = base.block_range_scanner.connect_ws::<N>(ws_url).await?;
        let mode = HistoricModeConfig { base, from_block, to_block };
        Ok(HistoricModeScanner { config: mode, inner: EventScannerService::from_config(brs) })
    }

    /// Connects to the provider via IPC
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<HistoricModeScanner<N>> {
        let HistoricModeConfig { base, from_block, to_block } = self;
        let brs = base.block_range_scanner.connect_ipc::<N>(ipc_path).await?;
        let mode = HistoricModeConfig { base, from_block, to_block };
        Ok(HistoricModeScanner { config: mode, inner: EventScannerService::from_config(brs) })
    }

    /// Connects to an existing provider
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub fn connect_provider<N: Network>(
        self,
        provider: RootProvider<N>,
    ) -> TransportResult<HistoricModeScanner<N>> {
        let HistoricModeConfig { base, from_block, to_block } = self;
        let brs = base.block_range_scanner.connect_provider::<N>(provider)?;
        let mode = HistoricModeConfig { base, from_block, to_block };
        Ok(HistoricModeScanner { config: mode, inner: EventScannerService::from_config(brs) })
    }
}

impl<N: Network> HistoricModeScanner<N> {
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
    pub async fn stream(self) -> Result<(), EventScannerError> {
        self.inner.stream_historical(self.config.from_block, self.config.to_block).await
    }
}
