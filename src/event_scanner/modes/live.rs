use alloy::{
    network::Network,
    providers::RootProvider,
    transports::{TransportResult, http::reqwest::Url},
};

use tokio_stream::wrappers::ReceiverStream;

use crate::{
    block_range_scanner::DEFAULT_BLOCK_CONFIRMATIONS,
    event_scanner::{
        EventScannerError,
        filter::EventFilter,
        scanner::{EventScannerMessage, EventScannerService},
    },
};

use super::{BaseConfig, BaseConfigBuilder};

pub struct LiveScannerBuilder {
    base: BaseConfig,
    // Defaults to 0
    block_confirmations: u64,
}

pub struct LiveEventScanner<N: Network> {
    config: LiveScannerBuilder,
    inner: EventScannerService<N>,
}

impl BaseConfigBuilder for LiveScannerBuilder {
    fn base_mut(&mut self) -> &mut BaseConfig {
        &mut self.base
    }
}

impl LiveScannerBuilder {
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
    pub async fn connect_ws<N: Network>(self, ws_url: Url) -> TransportResult<LiveEventScanner<N>> {
        let brs = self.base.block_range_scanner.connect_ws::<N>(ws_url).await?;
        Ok(LiveEventScanner { config: self, inner: EventScannerService::from_config(brs) })
    }

    /// Connects to the provider via IPC
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<LiveEventScanner<N>> {
        let brs = self.base.block_range_scanner.connect_ipc::<N>(ipc_path).await?;
        Ok(LiveEventScanner { config: self, inner: EventScannerService::from_config(brs) })
    }

    /// Connects to an existing provider
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    #[must_use]
    pub fn connect<N: Network>(
        self,
        provider: RootProvider<N>,
    ) -> LiveEventScanner<N> {
        let brs = self.base.block_range_scanner.connect::<N>(provider);
        LiveEventScanner { config: self, inner: EventScannerService::from_config(brs) }
    }
}

impl<N: Network> LiveEventScanner<N> {
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
    pub async fn start(self) -> Result<(), EventScannerError> {
        self.inner.stream_live(self.config.block_confirmations).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_live_scanner_config_defaults() {
        let config = LiveScannerBuilder::new();

        assert_eq!(config.block_confirmations, DEFAULT_BLOCK_CONFIRMATIONS);
    }

    #[test]
    fn test_live_scanner_builder_pattern() {
        let config = LiveScannerBuilder::new().block_confirmations(10).max_block_range(50);

        assert_eq!(config.block_confirmations, 10);
        assert_eq!(config.base.block_range_scanner.max_block_range, 50);
    }

    #[test]
    fn test_live_scanner_builder_pattern_chaining() {
        let config = LiveScannerBuilder::new().max_block_range(25).block_confirmations(5);

        assert_eq!(config.base.block_range_scanner.max_block_range, 25);
        assert_eq!(config.block_confirmations, 5);
    }

    #[test]
    fn test_live_scanner_builder_with_zero_confirmations() {
        let config = LiveScannerBuilder::new().block_confirmations(0).max_block_range(100);

        assert_eq!(config.block_confirmations, 0);
        assert_eq!(config.base.block_range_scanner.max_block_range, 100);
    }
}