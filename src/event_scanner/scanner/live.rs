use alloy::network::Network;

use super::common::{ConsumerMode, handle_stream};
use crate::{
    EventScannerBuilder, ScannerError,
    event_scanner::{EventScanner, scanner::Live},
    robust_provider::IntoRobustProvider,
};

impl EventScannerBuilder<Live> {
    /// Sets the number of confirmations required before a block is considered stable enough to
    /// scan in live mode.
    ///
    /// Higher values reduce the likelihood of emitting logs from blocks that are later reorged,
    /// at the cost of increased event delivery latency.
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
    /// Starts the scanner in [`Live`] mode.
    ///
    /// See [`EventScanner`] for general startup notes.
    ///
    /// # Errors
    ///
    /// * [`ScannerError::Timeout`] - if an RPC call required for startup times out.
    /// * [`ScannerError::RpcError`] - if an RPC call required for startup fails.
    pub async fn start(self) -> Result<(), ScannerError> {
        let stream = self.block_range_scanner.stream_live(self.config.block_confirmations).await?;
        let max_concurrent_fetches = self.config.max_concurrent_fetches;
        let provider = self.block_range_scanner.provider().clone();
        let listeners = self.listeners.clone();
        let buffer_capacity = self.buffer_capacity();

        tokio::spawn(async move {
            handle_stream(
                stream,
                &provider,
                &listeners,
                ConsumerMode::Stream,
                max_concurrent_fetches,
                buffer_capacity,
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
        block_range_scanner::{
            DEFAULT_BLOCK_CONFIRMATIONS, DEFAULT_MAX_BLOCK_RANGE, DEFAULT_STREAM_BUFFER_CAPACITY,
        },
        event_scanner::scanner::DEFAULT_MAX_CONCURRENT_FETCHES,
    };

    use super::*;

    #[test]
    fn test_live_scanner_builder_pattern() {
        let builder = EventScannerBuilder::live()
            .max_block_range(25)
            .block_confirmations(5)
            .max_concurrent_fetches(10)
            .buffer_capacity(33);

        assert_eq!(builder.block_range_scanner.max_block_range, 25);
        assert_eq!(builder.config.block_confirmations, 5);
        assert_eq!(builder.config.max_concurrent_fetches, 10);
        assert_eq!(builder.block_range_scanner.buffer_capacity, 33);
    }

    #[test]
    fn test_historic_scanner_builder_default_values() {
        let builder = EventScannerBuilder::live();

        assert_eq!(builder.config.block_confirmations, DEFAULT_BLOCK_CONFIRMATIONS);
        assert_eq!(builder.config.max_concurrent_fetches, DEFAULT_MAX_CONCURRENT_FETCHES);
        assert_eq!(builder.block_range_scanner.max_block_range, DEFAULT_MAX_BLOCK_RANGE);
        assert_eq!(builder.block_range_scanner.buffer_capacity, DEFAULT_STREAM_BUFFER_CAPACITY);
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
            .max_concurrent_fetches(20)
            .buffer_capacity(20)
            .buffer_capacity(40);

        assert_eq!(builder.block_range_scanner.max_block_range, 105);
        assert_eq!(builder.config.block_confirmations, 8);
        assert_eq!(builder.config.max_concurrent_fetches, 20);
        assert_eq!(builder.block_range_scanner.buffer_capacity, 40);
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

    #[tokio::test]
    async fn returns_error_with_zero_buffer_capacity() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let result = EventScannerBuilder::live().buffer_capacity(0).connect(provider).await;

        assert!(matches!(result, Err(ScannerError::InvalidBufferCapacity)));
    }
}
