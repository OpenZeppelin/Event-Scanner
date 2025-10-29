use alloy::network::Network;

use super::common::{ConsumerMode, handle_stream};
use crate::{
    EventScannerBuilder, ScannerError,
    event_scanner::{EventScanner, scanner::Live},
};

impl EventScannerBuilder<Live> {
    #[must_use]
    pub fn max_block_range(mut self, max_block_range: u64) -> Self {
        self.block_range_scanner.max_block_range = max_block_range;
        self
    }

    #[must_use]
    pub fn block_confirmations(mut self, confirmations: u64) -> Self {
        self.config.block_confirmations = confirmations;
        self
    }
}

impl<N: Network> EventScanner<Live, N> {
    /// Starts the scanner in live mode.
    ///
    /// Streams new blocks as they are produced, applying the configured
    /// `block_confirmations` to mitigate reorgs.
    ///
    /// # Reorg behavior
    ///
    /// - Emits [`ScannerStatus::ReorgDetected`] and adjusts the next confirmed range using
    ///   `block_confirmations` to re-emit the confirmed portion.
    ///
    /// # Errors
    ///
    /// Can error out if the service fails to start.
    pub async fn start(self) -> Result<(), ScannerError> {
        let client = self.block_range_scanner.run()?;
        let stream = client.stream_live(self.config.block_confirmations).await?;

        let provider = self.block_range_scanner.provider().clone();
        let listeners = self.listeners.clone();

        tokio::spawn(async move {
            handle_stream(stream, &provider, &listeners, ConsumerMode::Stream).await;
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_live_scanner_builder_pattern() {
        let config = EventScannerBuilder::live().max_block_range(25).block_confirmations(5);

        assert_eq!(config.block_range_scanner.max_block_range, 25);
        assert_eq!(config.config.block_confirmations, 5);
    }

    #[test]
    fn test_live_scanner_builder_with_zero_confirmations() {
        let config = EventScannerBuilder::live().block_confirmations(0).max_block_range(100);

        assert_eq!(config.config.block_confirmations, 0);
        assert_eq!(config.block_range_scanner.max_block_range, 100);
    }

    #[test]
    fn test_live_scanner_builder_last_call_wins() {
        let config = EventScannerBuilder::live()
            .max_block_range(25)
            .max_block_range(55)
            .max_block_range(105)
            .block_confirmations(2)
            .block_confirmations(4)
            .block_confirmations(8);

        assert_eq!(config.block_range_scanner.max_block_range, 105);
        assert_eq!(config.config.block_confirmations, 8);
    }
}
