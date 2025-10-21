use std::sync::Arc;

use alloy::{
    eips::BlockNumberOrTag,
    network::Ethereum,
    primitives::FixedBytes,
    providers::{Provider, ProviderBuilder, RootProvider, ext::AnvilApi},
    rpc::types::anvil::{ReorgOptions, TransactionData},
    sol,
    sol_types::SolEvent,
};
use alloy_node_bindings::{Anvil, AnvilInstance};
use event_scanner::{
    EventFilter, EventScanner, EventScannerMessage, HistoricEventScanner, LiveEventScanner,
    SyncEventScanner, event_lib::modes::LatestEventScanner,
};
use tokio_stream::wrappers::ReceiverStream;

// Shared test contract used across integration tests
sol! {
    // Built directly with solc 0.8.30+commit.73712a01.Darwin.appleclang
    #[sol(rpc, bytecode="608080604052346015576101b0908161001a8239f35b5f80fdfe6080806040526004361015610012575f80fd5b5f3560e01c90816306661abd1461016157508063a87d942c14610145578063d732d955146100ad5763e8927fbc14610048575f80fd5b346100a9575f3660031901126100a9575f5460018101809111610095576020817f7ca2ca9527391044455246730762df008a6b47bbdb5d37a890ef78394535c040925f55604051908152a1005b634e487b7160e01b5f52601160045260245ffd5b5f80fd5b346100a9575f3660031901126100a9575f548015610100575f198101908111610095576020817f53a71f16f53e57416424d0d18ccbd98504d42a6f98fe47b09772d8f357c620ce925f55604051908152a1005b60405162461bcd60e51b815260206004820152601860248201527f436f756e742063616e6e6f74206265206e6567617469766500000000000000006044820152606490fd5b346100a9575f3660031901126100a95760205f54604051908152f35b346100a9575f3660031901126100a9576020905f548152f3fea2646970667358221220471585b420a1ad0093820ff10129ec863f6df4bec186546249391fbc3cdbaa7c64736f6c634300081e0033")]
    contract TestCounter {
        uint256 public count;

        #[derive(Debug)]
        event CountIncreased(uint256 newCount);
        #[derive(Debug)]
        event CountDecreased(uint256 newCount);

        function increase() public {
            count += 1;
            emit CountIncreased(count);
        }

        function decrease() public {
            require(count > 0, "Count cannot be negative");
            count -= 1;
            emit CountDecreased(count);
        }

        function getCount() public view returns (uint256) {
            return count;
        }
    }
}

pub struct ScannerSetup<S, P>
where
    P: Provider<Ethereum> + Clone,
{
    pub provider: RootProvider,
    pub contract: TestCounter::TestCounterInstance<Arc<P>>,
    pub scanner: S,
    pub stream: ReceiverStream<EventScannerMessage>,
    pub anvil: AnvilInstance,
}

pub type LiveScannerSetup<P> = ScannerSetup<LiveEventScanner<Ethereum>, P>;
pub type HistoricScannerSetup<P> = ScannerSetup<HistoricEventScanner<Ethereum>, P>;
pub type SyncScannerSetup<P> = ScannerSetup<SyncEventScanner<Ethereum>, P>;
pub type LatestScannerSetup<P> = ScannerSetup<LatestEventScanner<Ethereum>, P>;

#[allow(clippy::missing_errors_doc)]
pub async fn setup_common(
    block_interval: Option<f64>,
    filter: Option<EventFilter>,
) -> anyhow::Result<(
    AnvilInstance,
    RootProvider,
    TestCounter::TestCounterInstance<Arc<RootProvider>>,
    EventFilter,
)> {
    let anvil = spawn_anvil(block_interval)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(Arc::new(provider.clone())).await?;

    let default_filter = EventFilter::new()
        .with_contract_address(*contract.address())
        .with_event(TestCounter::CountIncreased::SIGNATURE);

    let filter = filter.unwrap_or(default_filter);

    Ok((anvil, provider, contract, filter))
}

#[allow(clippy::missing_errors_doc)]
pub async fn setup_live_scanner(
    block_interval: Option<f64>,
    filter: Option<EventFilter>,
    confirmations: u64,
) -> anyhow::Result<LiveScannerSetup<impl Provider<Ethereum> + Clone>> {
    let (anvil, provider, contract, filter) = setup_common(block_interval, filter).await?;

    let mut scanner = EventScanner::live()
        .block_confirmations(confirmations)
        .connect_ws(anvil.ws_endpoint_url())
        .await?;

    let stream = scanner.create_event_stream(filter);

    Ok(ScannerSetup { provider, contract, scanner, stream, anvil })
}

#[allow(clippy::missing_errors_doc)]
pub async fn setup_sync_scanner(
    block_interval: Option<f64>,
    filter: Option<EventFilter>,
    confirmations: u64,
) -> anyhow::Result<SyncScannerSetup<impl Provider<Ethereum> + Clone>> {
    let (anvil, provider, contract, filter) = setup_common(block_interval, filter).await?;

    let mut scanner = EventScanner::sync()
        .block_confirmations(confirmations)
        .connect_ws(anvil.ws_endpoint_url())
        .await?;

    let stream = scanner.create_event_stream(filter);

    Ok(ScannerSetup { provider, contract, scanner, stream, anvil })
}

#[allow(clippy::missing_errors_doc)]
pub async fn setup_historic_scanner(
    block_interval: Option<f64>,
    filter: Option<EventFilter>,
    from: BlockNumberOrTag,
    to: BlockNumberOrTag,
) -> anyhow::Result<HistoricScannerSetup<impl Provider<Ethereum> + Clone>> {
    let (anvil, provider, contract, filter) = setup_common(block_interval, filter).await?;

    let mut scanner = EventScanner::historic()
        .from_block(from)
        .to_block(to)
        .connect_ws(anvil.ws_endpoint_url())
        .await?;

    let stream = scanner.create_event_stream(filter);

    Ok(ScannerSetup { provider, contract, scanner, stream, anvil })
}

#[allow(clippy::missing_errors_doc)]
pub async fn setup_latest_scanner(
    block_interval: Option<f64>,
    filter: Option<EventFilter>,
    count: usize,
    from: Option<BlockNumberOrTag>,
    to: Option<BlockNumberOrTag>,
) -> anyhow::Result<LatestScannerSetup<impl Provider<Ethereum> + Clone>> {
    let (anvil, provider, contract, filter) = setup_common(block_interval, filter).await?;
    let mut builder = EventScanner::latest().count(count);
    if let Some(f) = from {
        builder = builder.from_block(f);
    }
    if let Some(t) = to {
        builder = builder.to_block(t);
    }

    let mut scanner = builder.connect_ws(anvil.ws_endpoint_url()).await?;

    let stream = scanner.create_event_stream(filter);

    Ok(ScannerSetup { provider, contract, scanner, stream, anvil })
}

#[allow(clippy::missing_errors_doc)]
#[allow(clippy::missing_panics_doc)]
pub async fn reorg_with_new_count_incr_txs<P>(
    provider: RootProvider,
    contract: TestCounter::TestCounterInstance<Arc<P>>,
    num_initial_events: u64,
    num_new_events: u64,
    reorg_depth: u64,
    same_block: bool,
) -> anyhow::Result<Vec<FixedBytes<32>>>
where
    P: Provider<Ethereum> + Clone,
{
    let mut event_tx_hashes = vec![];

    for _ in 0..num_initial_events {
        let receipt = contract.increase().send().await.unwrap().get_receipt().await.unwrap();
        event_tx_hashes.push(receipt.transaction_hash);
    }

    let mut tx_block_pairs = vec![];
    for i in 0..num_new_events {
        let tx = contract.increase().into_transaction_request();
        tx_block_pairs.push((TransactionData::JSON(tx), if same_block { 0 } else { i }));
    }

    let pre_reorg_block = provider.get_block_by_number(BlockNumberOrTag::Latest).await?.unwrap();

    provider.anvil_reorg(ReorgOptions { depth: reorg_depth, tx_block_pairs }).await.unwrap();

    let post_reorg_block = provider
        .get_block_by_number(BlockNumberOrTag::Number(pre_reorg_block.number()))
        .full()
        .await?
        .unwrap();

    assert_eq!(post_reorg_block.header.number, pre_reorg_block.header.number);
    assert_ne!(post_reorg_block.header.hash, pre_reorg_block.header.hash);

    if same_block {
        let new_block = provider
            .get_block_by_number(BlockNumberOrTag::Number(
                post_reorg_block.header.number - reorg_depth + 1,
            ))
            .await?
            .unwrap();
        assert_eq!(new_block.transactions.len() as u64, num_new_events);
        for tx_hash in new_block.transactions.hashes() {
            event_tx_hashes.push(tx_hash);
        }
    } else {
        for i in 0..num_new_events {
            let new_block = provider
                .get_block_by_number(BlockNumberOrTag::Number(
                    post_reorg_block.header.number - reorg_depth + 1 + i,
                ))
                .await?
                .unwrap();
            assert_eq!(new_block.transactions.len() as u64, 1);
            for tx_hash in new_block.transactions.hashes() {
                event_tx_hashes.push(tx_hash);
            }
        }
    }

    Ok(event_tx_hashes)
}

#[allow(clippy::missing_errors_doc)]
pub fn spawn_anvil(block_time: Option<f64>) -> anyhow::Result<AnvilInstance> {
    let mut anvil = Anvil::new();
    if let Some(block_time) = block_time {
        anvil = anvil.block_time_f64(block_time);
    }
    Ok(anvil.try_spawn()?)
}

#[allow(clippy::missing_errors_doc)]
#[allow(clippy::missing_panics_doc)]
pub async fn build_provider(anvil: &AnvilInstance) -> anyhow::Result<RootProvider> {
    let wallet = anvil.wallet().expect("anvil should return a default wallet");
    let provider = ProviderBuilder::new().wallet(wallet).connect(anvil.endpoint().as_str()).await?;
    Ok(provider.root().to_owned())
}

#[allow(clippy::missing_errors_doc)]
pub async fn deploy_counter<P>(provider: P) -> anyhow::Result<TestCounter::TestCounterInstance<P>>
where
    P: alloy::providers::Provider<Ethereum> + Clone,
{
    let contract = TestCounter::deploy(provider).await?;
    Ok(contract)
}
