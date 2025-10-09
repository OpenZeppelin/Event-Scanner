use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    providers::RootProvider,
    rpc::client::ClientBuilder,
    transports::{TransportResult, http::reqwest::Url},
};

use crate::{
    block_range_scanner::{ConnectedBlockRangeScanner, DEFAULT_BLOCK_CONFIRMATIONS},
    event_lib::{Client, listener::EventListener},
};

use super::{BaseConfig, BaseConfigBuilder, EventStream};

pub struct SyncMode {
    base: BaseConfig,
    from_block: BlockNumberOrTag,
    block_confirmations: u64,
}

pub struct ConnectedSyncMode<N: Network> {
    block_range_scanner: ConnectedBlockRangeScanner<N>,
    event_listeners: Vec<EventListener>,
}

impl BaseConfigBuilder for SyncMode {
    fn base_mut(&mut self) -> &mut BaseConfig {
        &mut self.base
    }
}

impl SyncMode {
    fn new() -> Self {
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
    pub async fn connect_ws<N: Network>(
        self,
        ws_url: Url,
    ) -> TransportResult<ConnectedSyncMode<N>> {
        let block_range_scanner = self.base.block_range_scanner.connect_ws(ws_url).await?;
        let event_scanner =
            ConnectedSyncMode { block_range_scanner, event_listeners: Vec::default() };
        Ok(event_scanner)
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
        let block_range_scanner = self.base.block_range_scanner.connect_ipc(ipc_path).await?;

        let event_scanner =
            ConnectedSyncMode { block_range_scanner, event_listeners: Vec::default() };
        Ok(event_scanner)
    }

    pub fn connect_provider<N: Network>(
        self,
        provider: RootProvider<N>,
    ) -> TransportResult<ConnectedSyncMode<N>> {
        let block_range_scanner = self.base.block_range_scanner.connect_provider(provider)?;
        let event_scanner =
            ConnectedSyncMode { block_range_scanner, event_listeners: Vec::default() };
        Ok(event_scanner)
    }
}

impl<N: Network> ConnectedSyncMode<N> {
    pub fn stream(self) -> EventStream {}
}
