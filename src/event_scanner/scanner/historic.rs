use alloy::{eips::BlockNumberOrTag, network::Network};

use super::common::{ConsumerMode, handle_stream};
use crate::{
    EventScannerBuilder, ScannerError,
    event_scanner::scanner::{EventScanner, Historic},
};

impl EventScannerBuilder<Historic> {
    #[must_use]
    pub fn max_block_range(mut self, max_block_range: u64) -> Self {
        self.block_range_scanner.max_block_range = max_block_range;
        self
    }

    #[must_use]
    pub fn from_block(mut self, block: impl Into<BlockNumberOrTag>) -> Self {
        self.config.from_block = block.into();
        self
    }

    #[must_use]
    pub fn to_block(mut self, block: impl Into<BlockNumberOrTag>) -> Self {
        self.config.to_block = block.into();
        self
    }
}

impl<N: Network> EventScanner<Historic, N> {
    /// Starts the scanner.
    ///
    /// # Important notes
    ///
    /// * Register event streams via [`scanner.subscribe(filter)`][subscribe] **before** calling
    ///   this function.
    /// * The method returns immediately; events are delivered asynchronously.
    ///
    /// # Errors
    ///
    /// Can error out if the service fails to start.
    ///
    /// [subscribe]: EventScanner::subscribe
    pub async fn start(self) -> Result<(), ScannerError> {
        let client = self.block_range_scanner.run()?;
        let stream = client.stream_historical(self.config.from_block, self.config.to_block).await?;

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
    fn test_historic_scanner_builder_pattern() {
        let builder =
            EventScannerBuilder::historic().to_block(200).max_block_range(50).from_block(100);

        assert!(matches!(builder.config.from_block, BlockNumberOrTag::Number(100)));
        assert!(matches!(builder.config.to_block, BlockNumberOrTag::Number(200)));
        assert_eq!(builder.block_range_scanner.max_block_range, 50);
    }

    #[test]
    fn test_historic_scanner_builder_with_different_block_types() {
        let builder = EventScannerBuilder::historic()
            .from_block(BlockNumberOrTag::Earliest)
            .to_block(BlockNumberOrTag::Latest);

        assert!(matches!(builder.config.from_block, BlockNumberOrTag::Earliest));
        assert!(matches!(builder.config.to_block, BlockNumberOrTag::Latest));
    }

    #[test]
    fn test_historic_scanner_builder_last_call_wins() {
        let builder = EventScannerBuilder::historic()
            .max_block_range(25)
            .max_block_range(55)
            .max_block_range(105)
            .from_block(1)
            .from_block(2)
            .to_block(100)
            .to_block(200);

        assert_eq!(builder.block_range_scanner.max_block_range, 105);
        assert!(matches!(builder.config.from_block, BlockNumberOrTag::Number(2)));
        assert!(matches!(builder.config.to_block, BlockNumberOrTag::Number(200)));
    }
}
