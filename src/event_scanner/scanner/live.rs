use alloy::network::Network;

use super::common::{ConsumerMode, handle_stream};
use crate::{
    EventScannerBuilder, ScannerError,
    event_scanner::{EventScanner, scanner::Live},
    robust_provider::IntoRobustProvider,
};

impl EventScannerBuilder<Live> {
    #[must_use]
    pub fn block_confirmations(mut self, confirmations: u64) -> Self {
        self.config.block_confirmations = confirmations;
        self
    }

    /// Connects to an existing provider.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// * The provider connection fails
    /// * The max block range is zero
    pub async fn connect<N: Network>(
        self,
        provider: impl IntoRobustProvider<N>,
    ) -> Result<EventScanner<Live, N>, ScannerError> {
        self.build(provider).await
    }
}

impl<N: Network> EventScanner<Live, N> {
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
    use alloy::{
        network::Ethereum,
        providers::{RootProvider, mock::Asserter},
        rpc::client::RpcClient,
    };

    use super::*;

    #[test]
    fn test_live_scanner_builder_pattern() {
        let builder = EventScannerBuilder::live().max_block_range(25).block_confirmations(5);

        assert_eq!(builder.block_range_scanner.max_block_range, 25);
        assert_eq!(builder.config.block_confirmations, 5);
    }

    #[test]
    fn test_live_scanner_builder_with_zero_confirmations() {
        let builder = EventScannerBuilder::live().block_confirmations(0).max_block_range(100);

        assert_eq!(builder.config.block_confirmations, 0);
        assert_eq!(builder.block_range_scanner.max_block_range, 100);
    }

    #[test]
    fn test_live_scanner_builder_last_call_wins() {
        let builder = EventScannerBuilder::live()
            .max_block_range(25)
            .max_block_range(55)
            .max_block_range(105)
            .block_confirmations(2)
            .block_confirmations(4)
            .block_confirmations(8);

        assert_eq!(builder.block_range_scanner.max_block_range, 105);
        assert_eq!(builder.config.block_confirmations, 8);
    }

    #[tokio::test]
    async fn test_live_returns_error_with_zero_max_block_range() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let result = EventScannerBuilder::live().max_block_range(0).connect(provider).await;

        match result {
            Err(ScannerError::InvalidMaxBlockRange) => {}
            _ => panic!("Expected InvalidMaxBlockRange error"),
        }
    }
}
