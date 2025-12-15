use alloy::{eips::BlockId, network::Network};

use crate::{
    EventScannerBuilder, ScannerError,
    event_scanner::{
        EventScanner, SyncFromBlock,
        scanner::common::{ConsumerMode, handle_stream},
    },
    robust_provider::IntoRobustProvider,
};

impl EventScannerBuilder<SyncFromBlock> {
    #[must_use]
    pub fn block_confirmations(mut self, confirmations: u64) -> Self {
        self.config.block_confirmations = confirmations;
        self
    }

    /// Sets the maximum number of block-range fetches to process concurrently when
    /// synchronizing from a specific block.
    ///
    /// Higher values can improve throughput by issuing multiple RPC requests
    /// concurrently, at the cost of additional load on the provider.
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
    ) -> Result<EventScanner<SyncFromBlock, N>, ScannerError> {
        if self.config.max_concurrent_fetches == 0 {
            return Err(ScannerError::InvalidMaxConcurrentFetches);
        }

        let scanner = self.build(provider).await?;

        let provider = scanner.block_range_scanner.provider();

        if let BlockId::Hash(from_hash) = scanner.config.from_block {
            provider.get_block_by_hash(from_hash.into()).await?;
        }

        Ok(scanner)
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
        eips::{BlockId, BlockNumberOrTag},
        network::Ethereum,
        primitives::keccak256,
        providers::{Provider, ProviderBuilder, RootProvider, ext::AnvilApi, mock::Asserter},
        rpc::client::RpcClient,
    };
    use alloy_node_bindings::Anvil;

    use crate::{
        block_range_scanner::{DEFAULT_BLOCK_CONFIRMATIONS, DEFAULT_MAX_BLOCK_RANGE},
        event_scanner::scanner::DEFAULT_MAX_CONCURRENT_FETCHES,
    };

    use super::*;

    #[test]
    fn sync_scanner_builder_pattern() {
        let builder = EventScannerBuilder::sync()
            .from_block(50)
            .max_block_range(25)
            .block_confirmations(5)
            .max_concurrent_fetches(10);

        assert_eq!(builder.block_range_scanner.max_block_range, 25);
        assert_eq!(builder.config.block_confirmations, 5);
        assert_eq!(builder.config.max_concurrent_fetches, 10);
        assert_eq!(builder.config.from_block, BlockNumberOrTag::Number(50).into());
    }

    #[test]
    fn sync_scanner_builder_default_values() {
        let builder = EventScannerBuilder::sync().from_block(BlockNumberOrTag::Earliest);

        assert_eq!(builder.config.from_block, BlockNumberOrTag::Earliest.into());
        assert_eq!(builder.config.block_confirmations, DEFAULT_BLOCK_CONFIRMATIONS);
        assert_eq!(builder.config.max_concurrent_fetches, DEFAULT_MAX_CONCURRENT_FETCHES);
        assert_eq!(builder.block_range_scanner.max_block_range, DEFAULT_MAX_BLOCK_RANGE);
    }

    #[tokio::test]
    async fn accepts_zero_confirmations() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn().unwrap();
        let provider = ProviderBuilder::new().connect_http(anvil.endpoint_url());

        let scanner = EventScannerBuilder::sync()
            .from_block(BlockNumberOrTag::Earliest)
            .block_confirmations(0)
            .connect(provider)
            .await?;

        assert_eq!(scanner.config.block_confirmations, 0);

        Ok(())
    }

    #[test]
    fn sync_scanner_builder_last_call_wins() {
        let builder = EventScannerBuilder::sync()
            .from_block(2)
            .max_block_range(25)
            .max_block_range(55)
            .max_block_range(105)
            .block_confirmations(5)
            .block_confirmations(7)
            .max_concurrent_fetches(10)
            .max_concurrent_fetches(20);

        assert_eq!(builder.block_range_scanner.max_block_range, 105);
        assert_eq!(builder.config.from_block, BlockNumberOrTag::Number(2).into());
        assert_eq!(builder.config.block_confirmations, 7);
        assert_eq!(builder.config.max_concurrent_fetches, 20);
    }

    #[tokio::test]
    async fn returns_error_with_zero_max_concurrent_fetches() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let result = EventScannerBuilder::sync()
            .from_block(0)
            .max_concurrent_fetches(0)
            .connect(provider)
            .await;

        assert!(matches!(result, Err(ScannerError::InvalidMaxConcurrentFetches)));
    }

    #[tokio::test]
    async fn test_sync_from_block_returns_error_with_zero_max_block_range() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let result =
            EventScannerBuilder::sync().from_block(100).max_block_range(0).connect(provider).await;

        match result {
            Err(ScannerError::InvalidMaxBlockRange) => {}
            _ => panic!("Expected InvalidMaxBlockRange error"),
        }
    }

    #[tokio::test]
    async fn test_sync_from_block_scanner_with_valid_from_hash() {
        let anvil = Anvil::new().try_spawn().unwrap();
        let provider = ProviderBuilder::new().connect_http(anvil.endpoint_url());

        provider.anvil_mine(Some(5), None).await.unwrap();

        let block_5_hash =
            provider.get_block_by_number(5.into()).await.unwrap().unwrap().header.hash;

        let result =
            EventScannerBuilder::sync().from_block(block_5_hash).connect(provider.clone()).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_sync_from_block_scanner_with_invalid_from_hash() {
        let anvil = Anvil::new().try_spawn().unwrap();
        let provider = ProviderBuilder::new().connect_http(anvil.endpoint_url());

        let random_hash = keccak256("Invalid Hash");
        let result = EventScannerBuilder::sync().from_block(random_hash).connect(provider).await;

        match result {
            Err(ScannerError::BlockNotFound(id)) => {
                assert_eq!(id, BlockId::Hash(random_hash.into()));
            }
            Err(e) => panic!("Expected BlockNotFound error, got {e:?}"),
            Ok(_) => panic!("Expected error, but got Ok"),
        }
    }
}
