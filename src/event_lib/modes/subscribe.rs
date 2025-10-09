use alloy::{
    network::Network,
    providers::RootProvider,
    rpc::client::ClientBuilder,
    transports::{TransportResult, http::reqwest::Url},
};

use crate::block_range_scanner::DEFAULT_BLOCK_CONFIRMATIONS;

use super::{BaseConfig, BaseConfigBuilder, EventStream};

pub struct SubscribeMode {
    base: BaseConfig,
    block_confirmations: u64,
}

pub struct ConnectedSubscribeMode<N: Network> {
    _provider: RootProvider<N>,
}

impl BaseConfigBuilder for SubscribeMode {
    fn base_mut(&mut self) -> &mut BaseConfig {
        &mut self.base
    }
}

impl SubscribeMode {
    pub(super) fn new() -> Self {
        Self { base: BaseConfig::new(), block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS }
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
    ) -> TransportResult<ConnectedSubscribeMode<N>> {
        let provider = RootProvider::<N>::new(
            ClientBuilder::default().ws(alloy::providers::WsConnect::new(ws_url)).await?,
        );

        Ok(ConnectedSubscribeMode { _provider: provider })
    }

    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<ConnectedSubscribeMode<N>> {
        let provider = RootProvider::<N>::new(ClientBuilder::default().ipc(ipc_path.into()).await?);

        Ok(ConnectedSubscribeMode { _provider: provider })
    }

    pub fn connect<N: Network>(self, provider: RootProvider<N>) -> ConnectedSubscribeMode<N> {
        ConnectedSubscribeMode { _provider: provider }
    }
}

impl<N: Network> ConnectedSubscribeMode<N> {
    pub fn stream(self) -> EventStream {
        EventStream
    }
}
