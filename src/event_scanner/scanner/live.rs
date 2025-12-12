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

    /// Sets the maximum number of block-range fetches to process concurrently for
    /// live streaming.
    ///
    /// This knob primarily exists to handle an edge case: when the scanner
    /// falls significantly behind the head of the chain (for example due to a
    /// temporary outage or slow consumer), it can catch up faster by
    /// processing multiple block ranges in parallel. Increasing the value
    /// improves throughput at the expense of higher load on the provider.
    ///
    /// Must be greater than 0.
    ///
    /// Defaults to [`DEFAULT_MAX_CONCURRENT_FETCHES`][default].
    ///
    /// [default]: crate::event_scanner::scanner::DEFAULT_MAX_CONCURRENT_FETCHES
    #[must_use]
    pub fn max_concurrent_fetches(mut self, max_concurrent_fetches: usize) -> Self {
        self.config.max_concurrent_fetches = max_concurrent_fetches;
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
        if self.config.max_concurrent_fetches == 0 {
            return Err(ScannerError::InvalidMaxConcurrentFetches);
        }

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

        let max_concurrent_fetches = self.config.max_concurrent_fetches;
        let provider = self.block_range_scanner.provider().clone();
        let listeners = self.listeners.clone();

        tokio::spawn(async move {
            handle_stream(
                stream,
                &provider,
                &listeners,
                ConsumerMode::Stream,
                max_concurrent_fetches,
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
        providers::{ProviderBuilder, RootProvider, mock::Asserter},
        rpc::client::RpcClient,
    };
    use alloy_node_bindings::Anvil;

    use crate::{
        block_range_scanner::DEFAULT_BLOCK_CONFIRMATIONS,
        event_scanner::scanner::DEFAULT_MAX_CONCURRENT_FETCHES,
    };

    use super::*;

    #[test]
    fn test_live_scanner_builder_pattern() {
        let builder = EventScannerBuilder::live()
            .max_block_range(25)
            .block_confirmations(5)
            .max_concurrent_fetches(10);

        assert_eq!(builder.block_range_scanner.max_block_range, 25);
        assert_eq!(builder.config.block_confirmations, 5);
        assert_eq!(builder.config.max_concurrent_fetches, 10);
    }

    #[test]
    fn test_historic_scanner_builder_default_values() {
        let builder = EventScannerBuilder::live();

        assert_eq!(builder.config.block_confirmations, DEFAULT_BLOCK_CONFIRMATIONS);
        assert_eq!(builder.config.max_concurrent_fetches, DEFAULT_MAX_CONCURRENT_FETCHES);
    }

    #[test]
    fn test_live_scanner_builder_last_call_wins() {
        let builder = EventScannerBuilder::live()
            .max_block_range(25)
            .max_block_range(55)
            .max_block_range(105)
            .block_confirmations(2)
            .block_confirmations(4)
            .block_confirmations(8)
            .max_concurrent_fetches(10)
            .max_concurrent_fetches(20);

        assert_eq!(builder.block_range_scanner.max_block_range, 105);
        assert_eq!(builder.config.block_confirmations, 8);
        assert_eq!(builder.config.max_concurrent_fetches, 20);
    }

    #[tokio::test]
    async fn accepts_zero_confirmations() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn().unwrap();
        let provider = ProviderBuilder::new().connect_http(anvil.endpoint_url());

        let scanner = EventScannerBuilder::live().block_confirmations(0).connect(provider).await?;

        assert_eq!(scanner.config.block_confirmations, 0);

        Ok(())
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

    #[tokio::test]
    async fn returns_error_with_zero_max_concurrent_fetches() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let result = EventScannerBuilder::live().max_concurrent_fetches(0).connect(provider).await;

        assert!(matches!(result, Err(ScannerError::InvalidMaxConcurrentFetches)));
    }
}
