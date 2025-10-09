use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    providers::RootProvider,
    rpc::client::ClientBuilder,
    transports::{TransportResult, http::reqwest::Url},
};

use super::{BaseConfig, BaseConfigBuilder, EventStream};

pub struct HistoricMode {
    base: BaseConfig,
    from_block: BlockNumberOrTag,
    to_block: BlockNumberOrTag,
}

pub struct ConnectedHistoricMode<N: Network> {
    _provider: RootProvider<N>,
}

impl BaseConfigBuilder for HistoricMode {
    fn base_mut(&mut self) -> &mut BaseConfig {
        &mut self.base
    }
}

impl HistoricMode {
    pub(super) fn new() -> Self {
        Self {
            base: BaseConfig::new(),
            from_block: BlockNumberOrTag::Earliest,
            to_block: BlockNumberOrTag::Latest,
        }
    }

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
    ) -> TransportResult<ConnectedHistoricMode<N>> {
        let provider = RootProvider::<N>::new(
            ClientBuilder::default().ws(alloy::providers::WsConnect::new(ws_url)).await?,
        );

        Ok(ConnectedHistoricMode { _provider: provider })
    }

    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<ConnectedHistoricMode<N>> {
        let provider = RootProvider::<N>::new(ClientBuilder::default().ipc(ipc_path.into()).await?);

        Ok(ConnectedHistoricMode { _provider: provider })
    }

    pub fn connect<N: Network>(self, provider: RootProvider<N>) -> ConnectedHistoricMode<N> {
        ConnectedHistoricMode { _provider: provider }
    }
}

impl<N: Network> ConnectedHistoricMode<N> {
    pub fn stream(self) -> EventStream {
        EventStream
    }
}
