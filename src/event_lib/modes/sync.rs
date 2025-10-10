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
    pub fn connect_provider<N: Network>(
        self,
        provider: RootProvider<N>,
    ) -> TransportResult<SyncEventScanner<N>> {
        let SyncScannerConfig { base, from_block, block_confirmations } = self;
        let brs = base.block_range_scanner.connect_provider::<N>(provider)?;
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
    pub async fn stream(self) -> Result<(), EventScannerError> {
        self.inner.stream_from(self.config.from_block, self.config.block_confirmations).await
    }
}
