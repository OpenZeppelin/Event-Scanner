use alloy::{
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

pub struct SubscribeMode {
    base: BaseConfig,
    block_confirmations: u64,
}

pub struct ConnectedSubscribeMode<N: Network> {
    inner: ConnectedEventScanner<N>,
    block_confirmations: Option<u64>,
}

impl BaseConfigBuilder for SubscribeMode {
    fn base_mut(&mut self) -> &mut BaseConfig {
        &mut self.base
    }
}

impl SubscribeMode {
    pub(super) fn new() -> Self {
        Self {
            base: BaseConfig::new(),
            block_confirmations: crate::block_range_scanner::DEFAULT_BLOCK_CONFIRMATIONS,
        }
    }

    pub fn block_confirmations(mut self, count: u64) -> Self {
        self.block_confirmations = count;
        self
    }

    pub async fn connect_ws<N: Network>(
        self,
        ws_url: Url,
    ) -> TransportResult<ConnectedSubscribeMode<N>> {
        let brs = self.base.block_range_scanner.connect_ws::<N>(ws_url).await?;
        Ok(ConnectedSubscribeMode {
            inner: ConnectedEventScanner::from_connected(brs),
            block_confirmations: Some(self.block_confirmations),
        })
    }

    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<ConnectedSubscribeMode<N>> {
        let brs = self.base.block_range_scanner.connect_ipc::<N>(ipc_path).await?;
        Ok(ConnectedSubscribeMode {
            inner: ConnectedEventScanner::from_connected(brs),
            block_confirmations: Some(self.block_confirmations),
        })
    }

    pub fn connect_provider<N: Network>(
        self,
        provider: RootProvider<N>,
    ) -> TransportResult<ConnectedSubscribeMode<N>> {
        let brs = self.base.block_range_scanner.connect_provider::<N>(provider)?;
        Ok(ConnectedSubscribeMode {
            inner: ConnectedEventScanner::from_connected(brs),
            block_confirmations: Some(self.block_confirmations),
        })
    }
}

impl<N: Network> ConnectedSubscribeMode<N> {
    pub fn create_event_stream(
        &mut self,
        filter: EventFilter,
    ) -> ReceiverStream<EventScannerMessage> {
        self.inner.create_event_stream(filter)
    }

    pub async fn stream(self) -> Result<(), EventScannerError> {
        self.inner.stream_live(self.block_confirmations).await
    }
}
