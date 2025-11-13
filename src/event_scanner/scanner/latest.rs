use alloy::{eips::BlockNumberOrTag, network::Network};

use super::common::{ConsumerMode, handle_stream};
use crate::{
    EventScannerBuilder, ScannerError,
    event_scanner::{EventScanner, LatestEvents},
    robust_provider::IntoRobustProvider,
};

impl EventScannerBuilder<LatestEvents> {
    #[must_use]
    pub fn block_confirmations(mut self, confirmations: u64) -> Self {
        self.config.block_confirmations = confirmations;
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

    /// Connects to an existing provider.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// * The provider connection fails
    /// * The event count is zero
    /// * The max block range is zero
    pub async fn connect<N: Network>(
        self,
        provider: impl IntoRobustProvider<N>,
    ) -> Result<EventScanner<LatestEvents, N>, ScannerError> {
        if self.config.count == 0 {
            return Err(ScannerError::InvalidEventCount);
        }
        self.build(provider).await
    }
}

impl<N: Network> EventScanner<LatestEvents, N> {
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
        let stream = client.rewind(self.config.from_block, self.config.to_block).await?;

        let provider = self.block_range_scanner.provider().clone();
        let listeners = self.listeners.clone();

        tokio::spawn(async move {
            handle_stream(
                stream,
                &provider,
                &listeners,
                ConsumerMode::CollectLatest { count: self.config.count },
            )
            .await;
        });

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use alloy::{
        network::Ethereum,
        providers::{RootProvider, mock::Asserter},
        rpc::client::RpcClient,
    };

    use super::*;

    #[test]
    fn test_latest_scanner_builder_pattern() {
        let builder = EventScannerBuilder::latest(3)
            .max_block_range(25)
            .block_confirmations(5)
            .from_block(BlockNumberOrTag::Number(50))
            .to_block(BlockNumberOrTag::Number(150));

        assert_eq!(builder.block_range_scanner.max_block_range, 25);
        assert_eq!(builder.config.block_confirmations, 5);
        assert_eq!(builder.config.count, 3);
        assert!(matches!(builder.config.from_block, BlockNumberOrTag::Number(50)));
        assert!(matches!(builder.config.to_block, BlockNumberOrTag::Number(150)));
    }

    #[test]
    fn test_latest_scanner_builder_with_different_block_types() {
        let builder = EventScannerBuilder::latest(10)
            .from_block(BlockNumberOrTag::Earliest)
            .to_block(BlockNumberOrTag::Latest)
            .block_confirmations(20);

        assert!(matches!(builder.config.from_block, BlockNumberOrTag::Earliest));
        assert!(matches!(builder.config.to_block, BlockNumberOrTag::Latest));
        assert_eq!(builder.config.count, 10);
        assert_eq!(builder.config.block_confirmations, 20);
    }

    #[test]
    fn test_latest_scanner_builder_last_call_wins() {
        let builder = EventScannerBuilder::latest(3)
            .from_block(10)
            .from_block(20)
            .to_block(100)
            .to_block(200)
            .block_confirmations(5)
            .block_confirmations(7)
            .max_block_range(50)
            .max_block_range(60);

        assert_eq!(builder.config.count, 3);
        assert!(matches!(builder.config.from_block, BlockNumberOrTag::Number(20)));
        assert!(matches!(builder.config.to_block, BlockNumberOrTag::Number(200)));
        assert_eq!(builder.config.block_confirmations, 7);
        assert_eq!(builder.block_range_scanner.max_block_range, 60);
    }

    #[tokio::test]
    async fn test_latest_returns_error_with_zero_count() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let result = EventScannerBuilder::latest(0).connect(provider).await;

        match result {
            Err(ScannerError::InvalidEventCount) => {}
            _ => panic!("Expected InvalidEventCount error"),
        }
    }

    #[tokio::test]
    async fn test_latest_returns_error_with_zero_max_block_range() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let result = EventScannerBuilder::latest(10).max_block_range(0).connect(provider).await;

        match result {
            Err(ScannerError::InvalidMaxBlockRange) => {}
            _ => panic!("Expected InvalidMaxBlockRange error"),
        }
    }
}
