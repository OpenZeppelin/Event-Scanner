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

pub struct LatestScannerConfig {
    base: BaseConfig,
    // Defatuls to 1
    count: u64,
    // Defaults to Earliest
    from_block: BlockNumberOrTag,
    // Defaults to Latest
    to_block: BlockNumberOrTag,
    // Defaults to 0
    block_confirmations: u64,
    // Defaults to false
    switch_to_live: bool,
}

pub struct LatestEventScanner<N: Network> {
    #[allow(dead_code)]
    config: LatestScannerConfig,
    inner: EventScannerService<N>,
}

impl BaseConfigBuilder for LatestScannerConfig {
    fn base_mut(&mut self) -> &mut BaseConfig {
        &mut self.base
    }
}

impl LatestScannerConfig {
    pub(super) fn new() -> Self {
        Self {
            base: BaseConfig::new(),
            count: 1,
            from_block: BlockNumberOrTag::Earliest,
            to_block: BlockNumberOrTag::Latest,
            block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS,
            switch_to_live: false,
        }
    }

    #[must_use]
    pub fn block_confirmations(mut self, count: u64) -> Self {
        self.block_confirmations = count;
        self
    }

    #[must_use]
    pub fn count(mut self, count: u64) -> Self {
        self.count = count;
        self
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

    #[must_use]
    pub fn then_live(mut self) -> Self {
        self.switch_to_live = true;
        self
    }

    /// Connects to the provider via IPC
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ws<N: Network>(
        self,
        ws_url: Url,
    ) -> TransportResult<LatestEventScanner<N>> {
        let LatestScannerConfig {
            base,
            count,
            from_block,
            to_block,
            block_confirmations,
            switch_to_live,
        } = self;
        let brs = base.block_range_scanner.connect_ws::<N>(ws_url).await?;
        let config = LatestScannerConfig {
            base,
            count,
            from_block,
            to_block,
            block_confirmations,
            switch_to_live,
        };
        Ok(LatestEventScanner { config, inner: EventScannerService::from_config(brs) })
    }

    /// Connects to the provider via IPC
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<LatestEventScanner<N>> {
        let LatestScannerConfig {
            base,
            count,
            from_block,
            to_block,
            block_confirmations,
            switch_to_live,
        } = self;
        let brs = base.block_range_scanner.connect_ipc::<N>(ipc_path).await?;
        let config = LatestScannerConfig {
            base,
            count,
            from_block,
            to_block,
            block_confirmations,
            switch_to_live,
        };
        Ok(LatestEventScanner { config, inner: EventScannerService::from_config(brs) })
    }

    /// Connects to an existing provider
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub fn connect_provider<N: Network>(
        self,
        provider: RootProvider<N>,
    ) -> TransportResult<LatestEventScanner<N>> {
        let LatestScannerConfig {
            base,
            count,
            from_block,
            to_block,
            block_confirmations,
            switch_to_live,
        } = self;
        let brs = base.block_range_scanner.connect_provider::<N>(provider)?;
        let config = LatestScannerConfig {
            base,
            count,
            from_block,
            to_block,
            block_confirmations,
            switch_to_live,
        };
        Ok(LatestEventScanner { config, inner: EventScannerService::from_config(brs) })
    }
}

impl<N: Network> LatestEventScanner<N> {
    pub fn create_event_stream(
        &mut self,
        filter: EventFilter,
    ) -> ReceiverStream<EventScannerMessage> {
        self.inner.create_event_stream(filter)
    }

    /// WARN: unimplemented - will call stream latest
    ///
    /// # Errors
    ///
    /// * `EventScannerMessage::ServiceShutdown` - if the service is already shutting down.
    #[allow(clippy::unused_async)]
    pub async fn stream(self) -> Result<(), EventScannerError> {
        unimplemented!()
    }
}
