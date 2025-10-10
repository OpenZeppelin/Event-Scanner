use alloy::{
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

pub struct LiveModeConfig {
    base: BaseConfig,
    block_confirmations: u64,
}

pub struct LiveModeScanner<N: Network> {
    mode: LiveModeConfig,
    inner: EventScannerService<N>,
}

impl BaseConfigBuilder for LiveModeConfig {
    fn base_mut(&mut self) -> &mut BaseConfig {
        &mut self.base
    }
}

impl LiveModeConfig {
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
    pub async fn connect_ws<N: Network>(self, ws_url: Url) -> TransportResult<LiveModeScanner<N>> {
        let LiveModeConfig { base, block_confirmations } = self;
        let brs = base.block_range_scanner.connect_ws::<N>(ws_url).await?;
        let mode = LiveModeConfig { base, block_confirmations };
        Ok(LiveModeScanner { mode, inner: EventScannerService::from_config(brs) })
    }

    /// Connects to the provider via IPC
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<LiveModeScanner<N>> {
        let LiveModeConfig { base, block_confirmations } = self;
        let brs = base.block_range_scanner.connect_ipc::<N>(ipc_path).await?;
        let mode = LiveModeConfig { base, block_confirmations };
        Ok(LiveModeScanner { mode, inner: EventScannerService::from_config(brs) })
    }

    /// Connects to an existing provider
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub fn connect_provider<N: Network>(
        self,
        provider: RootProvider<N>,
    ) -> TransportResult<LiveModeScanner<N>> {
        let LiveModeConfig { base, block_confirmations } = self;
        let brs = base.block_range_scanner.connect_provider::<N>(provider)?;
        let mode = LiveModeConfig { base, block_confirmations };
        Ok(LiveModeScanner { mode, inner: EventScannerService::from_config(brs) })
    }
}

impl<N: Network> LiveModeScanner<N> {
    pub fn create_event_stream(
        &mut self,
        filter: EventFilter,
    ) -> ReceiverStream<EventScannerMessage> {
        self.inner.create_event_stream(filter)
    }

    /// Calls stream live
    ///
    /// # Errors
    ///
    /// * `EventScannerMessage::ServiceShutdown` - if the service is already shutting down.
    pub async fn stream(self) -> Result<(), EventScannerError> {
        self.inner.stream_live(self.mode.block_confirmations).await
    }
}
