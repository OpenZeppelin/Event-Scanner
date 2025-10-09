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

pub struct HistoricMode {
    base: BaseConfig,
    from_block: BlockNumberOrTag,
    to_block: BlockNumberOrTag,
}

pub struct ConnectedHistoricMode<N: Network> {
    inner: ConnectedEventScanner<N>,
    from_block: BlockNumberOrTag,
    to_block: BlockNumberOrTag,
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

    pub fn to_block(mut self, block: impl Into<BlockNumberOrTag>) -> Self {
        self.to_block = block.into();
        self
    }

    pub async fn connect_ws<N: Network>(
        self,
        ws_url: Url,
    ) -> TransportResult<ConnectedHistoricMode<N>> {
        let brs = self.base.block_range_scanner.connect_ws::<N>(ws_url).await?;
        Ok(ConnectedHistoricMode {
            inner: ConnectedEventScanner::from_connected(brs),
            from_block: self.from_block,
            to_block: self.to_block,
        })
    }

    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<ConnectedHistoricMode<N>> {
        let brs = self.base.block_range_scanner.connect_ipc::<N>(ipc_path).await?;
        Ok(ConnectedHistoricMode {
            inner: ConnectedEventScanner::from_connected(brs),
            from_block: self.from_block,
            to_block: self.to_block,
        })
    }

    pub fn connect_provider<N: Network>(
        self,
        provider: RootProvider<N>,
    ) -> TransportResult<ConnectedHistoricMode<N>> {
        let brs = self.base.block_range_scanner.connect_provider::<N>(provider)?;
        Ok(ConnectedHistoricMode {
            inner: ConnectedEventScanner::from_connected(brs),
            from_block: self.from_block,
            to_block: self.to_block,
        })
    }
}

impl<N: Network> ConnectedHistoricMode<N> {
    pub fn create_event_stream(
        &mut self,
        filter: EventFilter,
    ) -> ReceiverStream<EventScannerMessage> {
        self.inner.create_event_stream(filter)
    }

    pub async fn stream(self) -> Result<(), EventScannerError> {
        self.inner.stream_historical(self.from_block, self.to_block).await
    }
}
