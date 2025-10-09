use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    providers::RootProvider,
    transports::{TransportResult, http::reqwest::Url},
};

use tokio_stream::wrappers::ReceiverStream;

use crate::event_lib::{
    filter::EventFilter,
    scanner::{ConnectedEventScanner, EventScannerError, EventScannerMessage},
};

use super::{BaseConfig, BaseConfigBuilder};

pub struct SyncMode {
    base: BaseConfig,
    from_block: BlockNumberOrTag,
    block_confirmations: u64,
}

pub struct ConnectedSyncMode<N: Network> {
    inner: ConnectedEventScanner<N>,
    from_block: BlockNumberOrTag,
    block_confirmations: Option<u64>,
}

impl BaseConfigBuilder for SyncMode {
    fn base_mut(&mut self) -> &mut BaseConfig {
        &mut self.base
    }
}

impl SyncMode {
    pub(super) fn new() -> Self {
        Self {
            base: BaseConfig::new(),
            from_block: BlockNumberOrTag::Earliest,
            block_confirmations: crate::block_range_scanner::DEFAULT_BLOCK_CONFIRMATIONS,
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
    pub async fn connect_ws<N: Network>(
        self,
        ws_url: Url,
    ) -> TransportResult<ConnectedSyncMode<N>> {
        let brs = self.base.block_range_scanner.connect_ws::<N>(ws_url).await?;
        Ok(ConnectedSyncMode {
            inner: ConnectedEventScanner::from_connected(brs),
            from_block: self.from_block,
            block_confirmations: Some(self.block_confirmations),
        })
    }

    /// Connects to the provider via IPC
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<ConnectedSyncMode<N>> {
        let brs = self.base.block_range_scanner.connect_ipc::<N>(ipc_path).await?;
        Ok(ConnectedSyncMode {
            inner: ConnectedEventScanner::from_connected(brs),
            from_block: self.from_block,
            block_confirmations: Some(self.block_confirmations),
        })
    }

    pub fn connect_provider<N: Network>(
        self,
        provider: RootProvider<N>,
    ) -> TransportResult<ConnectedSyncMode<N>> {
        let brs = self.base.block_range_scanner.connect_provider::<N>(provider)?;
        Ok(ConnectedSyncMode {
            inner: ConnectedEventScanner::from_connected(brs),
            from_block: self.from_block,
            block_confirmations: Some(self.block_confirmations),
        })
    }
}

impl<N: Network> ConnectedSyncMode<N> {
    pub fn create_event_stream(
        &mut self,
        filter: EventFilter,
    ) -> ReceiverStream<EventScannerMessage> {
        self.inner.create_event_stream(filter)
    }

    pub async fn stream(self) -> Result<(), EventScannerError> {
        self.inner.stream_from(self.from_block, self.block_confirmations).await
    }
}
