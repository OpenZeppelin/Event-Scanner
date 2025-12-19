use alloy::{
    consensus::BlockHeader,
    eips::BlockId,
    network::{BlockResponse, Network},
};

use super::common::{ConsumerMode, handle_stream};
use crate::{
    EventScannerBuilder, ScannerError,
    event_scanner::{EventScanner, LatestEvents},
    robust_provider::IntoRobustProvider,
};

impl EventScannerBuilder<LatestEvents> {
    /// Sets the number of confirmations required before a block is considered stable enough to
    /// include when collecting the latest events.
    ///
    /// Higher values reduce the likelihood of emitting logs from blocks that are later reorged,
    /// at the cost of potentially excluding very recent events.
    #[must_use]
    pub fn block_confirmations(mut self, confirmations: u64) -> Self {
        self.config.block_confirmations = confirmations;
        self
    }

    /// Sets the starting block for the historic scan.
    ///
    /// # Note
    ///
    /// Although passing `BlockNumberOrTag::Pending` will compile, the subsequent call to
    /// `connect` will fail at runtime. See issue <https://github.com/OpenZeppelin/Event-Scanner/issues/244>
    #[must_use]
    pub fn from_block(mut self, block_id: impl Into<BlockId>) -> Self {
        self.config.from_block = block_id.into();
        self
    }

    /// Sets the starting block for the historic scan.
    ///
    /// # Note
    ///
    /// Although passing `BlockNumberOrTag::Pending` will compile, the subsequent call to
    /// `connect` will fail at runtime. See issue <https://github.com/OpenZeppelin/Event-Scanner/issues/244>
    #[must_use]
    pub fn to_block(mut self, block_id: impl Into<BlockId>) -> Self {
        self.config.to_block = block_id.into();
        self
    }

    /// Sets the maximum number of block-range fetches to process concurrently when
    /// collecting the latest events.
    ///
    /// Higher values can increase throughput by issuing multiple RPC requests
    /// concurrently, at the expense of more load on the provider.
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
    /// * The event count is zero
    /// * The max block range is zero
    pub async fn connect<N: Network>(
        self,
        provider: impl IntoRobustProvider<N>,
    ) -> Result<EventScanner<LatestEvents, N>, ScannerError> {
        if self.config.count == 0 {
            return Err(ScannerError::InvalidEventCount);
        }
        if self.config.max_concurrent_fetches == 0 {
            return Err(ScannerError::InvalidMaxConcurrentFetches);
        }

        let scanner = self.build(provider).await?;

        let provider = scanner.block_range_scanner.provider();
        let latest_block = provider.get_block_number().await?;

        let from_num = match scanner.config.from_block {
            BlockId::Number(from_block) => {
                if from_block.is_pending() {
                    return Err(ScannerError::BlockExceedsLatest(
                        "from_block",
                        latest_block + 1,
                        latest_block,
                    ));
                }
                // can safely unwrap to 0 because any other tag < latest block
                from_block.as_number().unwrap_or(0)
            }
            BlockId::Hash(from_hash) => {
                provider.get_block_by_hash(from_hash.into()).await?.header().number()
            }
        };

        if from_num > latest_block {
            Err(ScannerError::BlockExceedsLatest("from_block", from_num, latest_block))?;
        }

        let to_num = match scanner.config.to_block {
            BlockId::Number(to_block) => {
                if to_block.is_pending() {
                    return Err(ScannerError::BlockExceedsLatest(
                        "to_block",
                        latest_block + 1,
                        latest_block,
                    ));
                }
                // can safely unwrap to 0 because any other tag < latest block
                to_block.as_number().unwrap_or(0)
            }
            BlockId::Hash(to_hash) => {
                provider.get_block_by_hash(to_hash.into()).await?.header().number()
            }
        };

        if to_num > latest_block {
            Err(ScannerError::BlockExceedsLatest("to_block", to_num, latest_block))?;
        }

        Ok(scanner)
    }
}

impl<N: Network> EventScanner<LatestEvents, N> {
    /// Starts the scanner in [`LatestEvents`] mode.
    ///
    /// See [`EventScanner`] for general startup notes.
    ///
    /// # Errors
    ///
    /// * [`ScannerError::Timeout`] - if an RPC call required for startup times out.
    /// * [`ScannerError::RpcError`] - if an RPC call required for startup fails.
    /// * [`ScannerError::BlockNotFound`] - if `from_block` or `to_block` cannot be resolved.
    pub async fn start(mut self) -> Result<(), ScannerError> {
        let stream = self
            .block_range_scanner
            .stream_rewind(self.config.from_block, self.config.to_block)
            .await?;

        let max_concurrent_fetches = self.config.max_concurrent_fetches;
        let provider = self.block_range_scanner.provider().clone();
        let listeners = self.listeners.clone();
        let buffer_capacity = self.buffer_capacity();

        tokio::spawn(async move {
            handle_stream(
                stream,
                &provider,
                &listeners,
                ConsumerMode::CollectLatest { count: self.config.count },
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
    use crate::{
        DEFAULT_STREAM_BUFFER_CAPACITY,
        block_range_scanner::{DEFAULT_BLOCK_CONFIRMATIONS, DEFAULT_MAX_BLOCK_RANGE},
        event_scanner::scanner::DEFAULT_MAX_CONCURRENT_FETCHES,
    };

    use super::*;
    use alloy::{
        eips::BlockNumberOrTag,
        network::Ethereum,
        primitives::keccak256,
        providers::{Provider, ProviderBuilder, RootProvider, ext::AnvilApi, mock::Asserter},
        rpc::client::RpcClient,
    };
    use alloy_node_bindings::Anvil;

    #[test]
    fn test_latest_scanner_builder_pattern() {
        let builder = EventScannerBuilder::latest(3)
            .max_block_range(25)
            .block_confirmations(5)
            .from_block(BlockNumberOrTag::Number(50))
            .to_block(BlockNumberOrTag::Number(150))
            .max_concurrent_fetches(10)
            .buffer_capacity(33);

        assert_eq!(builder.block_range_scanner.max_block_range, 25);
        assert_eq!(builder.config.block_confirmations, 5);
        assert_eq!(builder.config.max_concurrent_fetches, 10);
        assert_eq!(builder.config.count, 3);
        assert_eq!(builder.config.from_block, BlockNumberOrTag::Number(50).into());
        assert_eq!(builder.config.to_block, BlockNumberOrTag::Number(150).into());
        assert_eq!(builder.block_range_scanner.buffer_capacity, 33);
    }

    #[test]
    fn test_latest_scanner_builder_with_default_values() {
        let builder = EventScannerBuilder::latest(10);

        assert_eq!(builder.config.from_block, BlockNumberOrTag::Latest.into());
        assert_eq!(builder.config.to_block, BlockNumberOrTag::Earliest.into());
        assert_eq!(builder.config.count, 10);
        assert_eq!(builder.config.max_concurrent_fetches, DEFAULT_MAX_CONCURRENT_FETCHES);
        assert_eq!(builder.config.block_confirmations, DEFAULT_BLOCK_CONFIRMATIONS);
        assert_eq!(builder.block_range_scanner.max_block_range, DEFAULT_MAX_BLOCK_RANGE);
        assert_eq!(builder.block_range_scanner.buffer_capacity, DEFAULT_STREAM_BUFFER_CAPACITY);
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
            .max_block_range(60)
            .max_concurrent_fetches(10)
            .max_concurrent_fetches(20)
            .buffer_capacity(20)
            .buffer_capacity(40);

        assert_eq!(builder.config.count, 3);
        assert_eq!(builder.config.from_block, BlockNumberOrTag::Number(20).into());
        assert_eq!(builder.config.to_block, BlockNumberOrTag::Number(200).into());
        assert_eq!(builder.config.block_confirmations, 7);
        assert_eq!(builder.config.max_concurrent_fetches, 20);
        assert_eq!(builder.block_range_scanner.max_block_range, 60);
        assert_eq!(builder.block_range_scanner.buffer_capacity, 40);
    }

    #[tokio::test]
    async fn accepts_zero_confirmations() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn().unwrap();
        let provider = ProviderBuilder::new().connect_http(anvil.endpoint_url());

        let scanner =
            EventScannerBuilder::latest(1).block_confirmations(0).connect(provider).await?;

        assert_eq!(scanner.config.block_confirmations, 0);

        Ok(())
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

    #[tokio::test]
    async fn returns_error_with_zero_buffer_capacity() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let result = EventScannerBuilder::latest(10).buffer_capacity(0).connect(provider).await;

        assert!(matches!(result, Err(ScannerError::InvalidBufferCapacity)));
    }

    #[tokio::test]
    async fn returns_error_with_zero_max_concurrent_fetches() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let result =
            EventScannerBuilder::latest(10).max_concurrent_fetches(0).connect(provider).await;

        assert!(matches!(result, Err(ScannerError::InvalidMaxConcurrentFetches)));
    }

    #[tokio::test]
    async fn test_latest_scanner_with_valid_block_hash() {
        let anvil = Anvil::new().try_spawn().unwrap();
        let provider = ProviderBuilder::new().connect_http(anvil.endpoint_url());

        provider.anvil_mine(Some(5), None).await.unwrap();

        let block_1_hash =
            provider.get_block_by_number(1.into()).await.unwrap().unwrap().header.hash;
        let block_5_hash =
            provider.get_block_by_number(5.into()).await.unwrap().unwrap().header.hash;

        let result = EventScannerBuilder::latest(1)
            .from_block(block_1_hash)
            .to_block(block_5_hash)
            .connect(provider.clone())
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_latest_scanner_with_invalid_to_hash() {
        let anvil = Anvil::new().try_spawn().unwrap();
        let provider = ProviderBuilder::new().connect_http(anvil.endpoint_url());

        let random_hash = keccak256("Invalid Hash");
        let result = EventScannerBuilder::latest(1).to_block(random_hash).connect(provider).await;

        match result {
            Err(ScannerError::BlockNotFound(id)) => {
                assert_eq!(id, BlockId::Hash(random_hash.into()));
            }
            Err(e) => panic!("Expected BlockNotFound error, got {e:?}"),
            Ok(_) => panic!("Expected error, but got Ok"),
        }
    }

    #[tokio::test]
    async fn test_latest_scanner_with_invalid_from_hash() {
        let anvil = Anvil::new().try_spawn().unwrap();
        let provider = ProviderBuilder::new().connect_http(anvil.endpoint_url());

        let random_hash = keccak256("Invalid Hash");
        let result = EventScannerBuilder::latest(1).from_block(random_hash).connect(provider).await;

        match result {
            Err(ScannerError::BlockNotFound(id)) => {
                assert_eq!(id, BlockId::Hash(random_hash.into()));
            }
            Err(e) => panic!("Expected BlockNotFound error, got {e:?}"),
            Ok(_) => panic!("Expected error, but got Ok"),
        }
    }

    #[tokio::test]
    async fn test_latest_scanner_with_invalid_from_and_to_hash() {
        let anvil = Anvil::new().try_spawn().unwrap();
        let provider = ProviderBuilder::new().connect_http(anvil.endpoint_url());

        let random_from_hash = keccak256("Invalid From Hash");
        let random_to_hash = keccak256("Invalid To Hash");

        let result = EventScannerBuilder::latest(1)
            .from_block(random_from_hash)
            .to_block(random_to_hash)
            .connect(provider)
            .await;

        // We expect it to fail on the first checked block (from_block)
        match result {
            Err(ScannerError::BlockNotFound(id)) => {
                assert_eq!(id, BlockId::Hash(random_from_hash.into()));
            }
            Err(e) => panic!("Expected BlockNotFound error, got {e:?}"),
            Ok(_) => panic!("Expected error, but got Ok"),
        }
    }

    #[tokio::test]
    async fn test_latest_scanner_with_mixed_block_types() {
        let anvil = Anvil::new().try_spawn().unwrap();
        let provider = ProviderBuilder::new().connect_http(anvil.endpoint_url());

        provider.anvil_mine(Some(5), None).await.unwrap();

        let block_1_hash =
            provider.get_block_by_number(1.into()).await.unwrap().unwrap().header.hash;
        let block_5_hash =
            provider.get_block_by_number(5.into()).await.unwrap().unwrap().header.hash;

        let result = EventScannerBuilder::latest(1)
            .from_block(block_1_hash)
            .to_block(5)
            .connect(provider.clone())
            .await;

        assert!(result.is_ok());

        let result = EventScannerBuilder::latest(1)
            .from_block(1)
            .to_block(block_5_hash)
            .connect(provider)
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_from_block_above_latest_returns_error() {
        let anvil = Anvil::new().try_spawn().unwrap();
        let provider = ProviderBuilder::new().connect_http(anvil.endpoint_url());

        let latest_block = provider.get_block_number().await.unwrap();

        let result = EventScannerBuilder::latest(1)
            .from_block(latest_block + 100)
            .to_block(latest_block)
            .connect(provider)
            .await;

        match result {
            Err(ScannerError::BlockExceedsLatest("from_block", max, latest)) => {
                assert_eq!(max, latest_block + 100);
                assert_eq!(latest, latest_block);
            }
            _ => panic!("Expected BlockExceedsLatest error"),
        }
    }

    #[tokio::test]
    async fn test_to_block_above_latest_returns_error() {
        let anvil = Anvil::new().try_spawn().unwrap();
        let provider = ProviderBuilder::new().connect_http(anvil.endpoint_url());

        let latest_block = provider.get_block_number().await.unwrap();

        let result = EventScannerBuilder::latest(1)
            .from_block(0)
            .to_block(latest_block + 100)
            .connect(provider)
            .await;

        match result {
            Err(ScannerError::BlockExceedsLatest("to_block", max, latest)) => {
                assert_eq!(max, latest_block + 100);
                assert_eq!(latest, latest_block);
            }
            _ => panic!("Expected BlockExceedsLatest error"),
        }
    }

    #[tokio::test]
    async fn test_to_and_from_block_above_latest_returns_error() {
        let anvil = Anvil::new().try_spawn().unwrap();
        let provider = ProviderBuilder::new().connect_http(anvil.endpoint_url());

        let latest_block = provider.get_block_number().await.unwrap();

        let result = EventScannerBuilder::latest(1)
            .from_block(latest_block + 50)
            .to_block(latest_block + 100)
            .connect(provider)
            .await;

        match result {
            Err(ScannerError::BlockExceedsLatest("from_block", max, latest)) => {
                assert_eq!(max, latest_block + 50);
                assert_eq!(latest, latest_block);
            }
            _ => panic!("Expected BlockExceedsLatest error for 'from_block'"),
        }
    }

    #[tokio::test]
    async fn test_from_block_pending_returns_error() {
        let anvil = Anvil::new().try_spawn().unwrap();
        let provider = ProviderBuilder::new().connect_http(anvil.endpoint_url());

        let latest_block = provider.get_block_number().await.unwrap();

        let result = EventScannerBuilder::latest(1)
            .from_block(BlockNumberOrTag::Pending)
            .to_block(latest_block)
            .connect(provider)
            .await;

        match result {
            Err(ScannerError::BlockExceedsLatest("from_block", max, latest)) => {
                assert_eq!(max, latest_block + 1);
                assert_eq!(latest, latest_block);
            }
            _ => panic!("Expected BlockExceedsLatest error for 'from_block'"),
        }
    }

    #[tokio::test]
    async fn test_to_block_pending_returns_error() {
        let anvil = Anvil::new().try_spawn().unwrap();
        let provider = ProviderBuilder::new().connect_http(anvil.endpoint_url());

        let latest_block = provider.get_block_number().await.unwrap();

        let result = EventScannerBuilder::latest(1)
            .from_block(0)
            .to_block(BlockNumberOrTag::Pending)
            .connect(provider)
            .await;

        match result {
            Err(ScannerError::BlockExceedsLatest("to_block", max, latest)) => {
                assert_eq!(max, latest_block + 1);
                assert_eq!(latest, latest_block);
            }
            _ => panic!("Expected BlockExceedsLatest error for 'to_block'"),
        }
    }

    #[tokio::test]
    async fn test_from_and_to_block_pending_returns_error() {
        let anvil = Anvil::new().try_spawn().unwrap();
        let provider = ProviderBuilder::new().connect_http(anvil.endpoint_url());

        let latest_block = provider.get_block_number().await.unwrap();

        let result = EventScannerBuilder::latest(1)
            .from_block(BlockNumberOrTag::Pending)
            .to_block(BlockNumberOrTag::Pending)
            .connect(provider)
            .await;

        // from_block is checked first
        match result {
            Err(ScannerError::BlockExceedsLatest("from_block", max, latest)) => {
                assert_eq!(max, latest_block + 1);
                assert_eq!(latest, latest_block);
            }
            _ => panic!("Expected BlockExceedsLatest error for 'from_block'"),
        }
    }
}
