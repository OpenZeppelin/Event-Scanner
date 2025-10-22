use alloy::{
    network::Network,
    providers::RootProvider,
    transports::{TransportResult, http::reqwest::Url},
};

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::{
    block_range_scanner::{
        ConnectedBlockRangeScanner, DEFAULT_BLOCK_CONFIRMATIONS, MAX_BUFFERED_MESSAGES,
    },
    event_scanner::{
        EventScannerError,
        consumer::{ConsumerMode, handle_stream},
        filter::EventFilter,
        listener::EventListener,
        message::EventScannerMessage,
    },
};

use super::{BaseConfig, BaseConfigBuilder};

pub struct LiveScannerBuilder {
    base: BaseConfig,
    // Defaults to DEFAULT_BLOCK_CONFIRMATIONS
    block_confirmations: u64,
}

pub struct LiveEventScanner<N: Network> {
    config: LiveScannerBuilder,
    block_range_scanner: ConnectedBlockRangeScanner<N>,
    listeners: Vec<EventListener>,
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
        let block_range_scanner = self.base.block_range_scanner.connect_ws::<N>(ws_url).await?;
        Ok(LiveEventScanner { config: self, block_range_scanner, listeners: Vec::new() })
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
        let block_range_scanner = self.base.block_range_scanner.connect_ipc::<N>(ipc_path).await?;
        Ok(LiveEventScanner { config: self, block_range_scanner, listeners: Vec::new() })
    }

    /// Connects to an existing provider
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    #[must_use]
    pub fn connect<N: Network>(self, provider: RootProvider<N>) -> LiveEventScanner<N> {
        let block_range_scanner = self.base.block_range_scanner.connect::<N>(provider);
        LiveEventScanner { config: self, block_range_scanner, listeners: Vec::new() }
    }
}

impl<N: Network> LiveEventScanner<N> {
    pub fn create_event_stream(
        &mut self,
        filter: EventFilter,
    ) -> ReceiverStream<EventScannerMessage> {
        let (sender, receiver) = mpsc::channel::<EventScannerMessage>(MAX_BUFFERED_MESSAGES);
        self.listeners.push(EventListener { filter, sender });
        ReceiverStream::new(receiver)
    }

    /// Calls stream live
    ///
    /// # Errors
    ///
    /// * `EventScannerMessage::ServiceShutdown` - if the service is already shutting down.
    pub async fn start(self) -> Result<(), EventScannerError> {
        let client = self.block_range_scanner.run()?;
        let stream = client.stream_live(self.config.block_confirmations).await?;
        handle_stream(
            stream,
            self.block_range_scanner.provider(),
            &self.listeners,
            ConsumerMode::Stream,
        )
        .await;
        Ok(())
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
