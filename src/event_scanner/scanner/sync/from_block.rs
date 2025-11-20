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
        eips::{BlockId, BlockNumberOrTag},
        network::Ethereum,
        primitives::keccak256,
        providers::{Provider, ProviderBuilder, RootProvider, ext::AnvilApi, mock::Asserter},
        rpc::client::RpcClient,
    };
    use alloy_node_bindings::Anvil;

    use super::*;

    #[test]
    fn sync_scanner_builder_pattern() {
        let builder =
            EventScannerBuilder::sync().from_block(50).max_block_range(25).block_confirmations(5);

        assert_eq!(builder.block_range_scanner.max_block_range, 25);
        assert_eq!(builder.config.block_confirmations, 5);
        assert_eq!(builder.config.from_block, BlockNumberOrTag::Number(50).into());
    }

    #[test]
    fn sync_scanner_builder_with_different_block_types() {
        let builder = EventScannerBuilder::sync()
            .from_block(BlockNumberOrTag::Earliest)
            .block_confirmations(20)
            .max_block_range(100);

        assert_eq!(builder.config.from_block, BlockNumberOrTag::Earliest.into());
        assert_eq!(builder.config.block_confirmations, 20);
        assert_eq!(builder.block_range_scanner.max_block_range, 100);
    }

    #[test]
    fn sync_scanner_builder_with_zero_confirmations() {
        let builder =
            EventScannerBuilder::sync().from_block(0).block_confirmations(0).max_block_range(75);

        assert_eq!(builder.config.from_block, BlockNumberOrTag::Number(0).into());
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
        assert_eq!(builder.config.from_block, BlockNumberOrTag::Number(2).into());
        assert_eq!(builder.config.block_confirmations, 7);
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
