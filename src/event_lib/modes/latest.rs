use alloy::{
    network::Network,
    providers::RootProvider,
    rpc::client::ClientBuilder,
    transports::{TransportResult, http::reqwest::Url},
};

use super::{BaseConfig, BaseConfigBuilder, EventStream};

pub struct LatestMode {
    base: BaseConfig,
    count: u64,
}

pub struct ConnectedLatestMode<N: Network> {
    _provider: RootProvider<N>,
}

impl BaseConfigBuilder for LatestMode {
    fn base_mut(&mut self) -> &mut BaseConfig {
        &mut self.base
    }
}

impl LatestMode {
    fn new(count: u64) -> Self {
        Self { base: BaseConfig::new(), count }
    }

    /// Connects to the provider via WebSocket
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ws<N: Network>(
        self,
        ws_url: Url,
    ) -> TransportResult<ConnectedLatestMode<N>> {
        let provider = RootProvider::<N>::new(
            ClientBuilder::default().ws(alloy::providers::WsConnect::new(ws_url)).await?,
        );

        Ok(ConnectedLatestMode { _provider: provider })
    }

    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<ConnectedLatestMode<N>> {
        let provider = RootProvider::<N>::new(ClientBuilder::default().ipc(ipc_path.into()).await?);

        Ok(ConnectedLatestMode { _provider: provider })
    }

    pub fn connect<N: Network>(self, provider: RootProvider<N>) -> ConnectedLatestMode<N> {
        ConnectedLatestMode { _provider: provider }
    }
}

impl<N: Network> ConnectedLatestMode<N> {
    pub fn stream(self) -> EventStream {
        EventStream
    }
}
