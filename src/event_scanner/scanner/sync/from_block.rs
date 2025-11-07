use alloy::network::Network;

use crate::{
    EventScannerBuilder, ScannerError,
    event_scanner::{
        EventScanner, SyncFromBlock,
        scanner::common::{ConsumerMode, handle_stream},
    },
};

impl EventScannerBuilder<SyncFromBlock> {
    #[must_use]
    pub fn block_confirmations(mut self, confirmations: u64) -> Self {
        self.config.block_confirmations = confirmations;
        self
    }
}

impl<N: Network> EventScanner<SyncFromBlock, N> {
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
        let stream =
            client.stream_from(self.config.from_block, self.config.block_confirmations).await?;

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
    use alloy::eips::BlockNumberOrTag;

    use super::*;

    #[test]
    fn sync_scanner_builder_pattern() {
        let builder =
            EventScannerBuilder::sync().from_block(50).max_block_range(25).block_confirmations(5);

        assert_eq!(builder.block_range_scanner.max_block_range, 25);
        assert_eq!(builder.config.block_confirmations, 5);
        assert!(matches!(builder.config.from_block, BlockNumberOrTag::Number(50)));
    }

    #[test]
    fn sync_scanner_builder_with_different_block_types() {
        let builder = EventScannerBuilder::sync()
            .from_block(BlockNumberOrTag::Earliest)
            .block_confirmations(20)
            .max_block_range(100);

        assert!(matches!(builder.config.from_block, BlockNumberOrTag::Earliest));
        assert_eq!(builder.config.block_confirmations, 20);
        assert_eq!(builder.block_range_scanner.max_block_range, 100);
    }

    #[test]
    fn sync_scanner_builder_with_zero_confirmations() {
        let builder =
            EventScannerBuilder::sync().from_block(0).block_confirmations(0).max_block_range(75);

        assert!(matches!(builder.config.from_block, BlockNumberOrTag::Number(0)));
        assert_eq!(builder.config.block_confirmations, 0);
        assert_eq!(builder.block_range_scanner.max_block_range, 75);
    }

    #[test]
    fn sync_scanner_builder_last_call_wins() {
        let builder = EventScannerBuilder::sync()
            .from_block(2)
            .max_block_range(25)
            .max_block_range(55)
            .max_block_range(105)
            .block_confirmations(5)
            .block_confirmations(7);

        assert_eq!(builder.block_range_scanner.max_block_range, 105);
        assert!(matches!(builder.config.from_block, BlockNumberOrTag::Number(2)));
        assert_eq!(builder.config.block_confirmations, 7);
    }
}
