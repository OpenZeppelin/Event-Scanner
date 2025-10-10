use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    providers::RootProvider,
    transports::{TransportResult, http::reqwest::Url},
};

use tokio_stream::wrappers::ReceiverStream;

use crate::{
    block_range_scanner::DEFAULT_BLOCK_CONFIRMATIONS,
    event_lib::{
        filter::EventFilter,
        scanner::{EventScannerError, EventScannerMessage, EventScannerService},
    },
};

use super::{BaseConfig, BaseConfigBuilder};

pub struct SyncModeConfig {
    base: BaseConfig,
    from_block: BlockNumberOrTag,
    block_confirmations: u64,
}

pub struct SyncModeScanner<N: Network> {
    mode: SyncModeConfig,
    inner: EventScannerService<N>,
}

impl BaseConfigBuilder for SyncModeConfig {
    fn base_mut(&mut self) -> &mut BaseConfig {
        &mut self.base
    }
}

impl SyncModeConfig {
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
    pub async fn connect_ws<N: Network>(self, ws_url: Url) -> TransportResult<SyncModeScanner<N>> {
        let SyncModeConfig { base, from_block, block_confirmations } = self;
        let brs = base.block_range_scanner.connect_ws::<N>(ws_url).await?;
        let mode = SyncModeConfig { base, from_block, block_confirmations };
        Ok(SyncModeScanner { mode, inner: EventScannerService::from_config(brs) })
    }

    /// Connects to the provider via IPC
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<SyncModeScanner<N>> {
        let SyncModeConfig { base, from_block, block_confirmations } = self;
        let brs = base.block_range_scanner.connect_ipc::<N>(ipc_path).await?;
        let mode = SyncModeConfig { base, from_block, block_confirmations };
        Ok(SyncModeScanner { mode, inner: EventScannerService::from_config(brs) })
    }

    pub fn connect_provider<N: Network>(
        self,
        provider: RootProvider<N>,
    ) -> TransportResult<SyncModeScanner<N>> {
        let SyncModeConfig { base, from_block, block_confirmations } = self;
        let brs = base.block_range_scanner.connect_provider::<N>(provider)?;
        let mode = SyncModeConfig { base, from_block, block_confirmations };
        Ok(SyncModeScanner { mode, inner: EventScannerService::from_config(brs) })
    }
}

impl<N: Network> SyncModeScanner<N> {
    pub fn create_event_stream(
        &mut self,
        filter: EventFilter,
    ) -> ReceiverStream<EventScannerMessage> {
        self.inner.create_event_stream(filter)
    }

    pub async fn stream(self) -> Result<(), EventScannerError> {
        self.inner.stream_from(self.mode.from_block, self.mode.block_confirmations).await
    }
}
