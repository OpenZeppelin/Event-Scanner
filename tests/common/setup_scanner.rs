use alloy::{
    eips::BlockNumberOrTag,
    network::Ethereum,
    providers::{Provider, RootProvider},
    sol_types::SolEvent,
};
use alloy_node_bindings::AnvilInstance;
use event_scanner::{
    EventFilter, EventScanner, EventScannerBuilder, Historic, LatestEvents, Live, Message,
    SyncFromBlock, SyncFromLatestEvents, robust_provider::RobustProvider,
};
use tokio_stream::wrappers::ReceiverStream;

use crate::common::{
    TestCounter::{self, CountIncreased},
    build_provider, spawn_anvil,
    test_counter::deploy_counter,
};

pub struct ScannerSetup<S, P>
where
    P: Provider<Ethereum> + Clone,
{
    pub provider: RobustProvider<Ethereum>,
    pub contract: TestCounter::TestCounterInstance<P>,
    pub scanner: S,
    pub stream: ReceiverStream<Message>,
    #[allow(dead_code)]
    pub anvil: AnvilInstance,
}

pub type LiveScannerSetup<P> = ScannerSetup<EventScanner<Live>, P>;
pub type HistoricScannerSetup<P> = ScannerSetup<EventScanner<Historic>, P>;
pub type SyncScannerSetup<P> = ScannerSetup<EventScanner<SyncFromBlock>, P>;
pub type SyncFromLatestScannerSetup<P> = ScannerSetup<EventScanner<SyncFromLatestEvents>, P>;
pub type LatestScannerSetup<P> = ScannerSetup<EventScanner<LatestEvents>, P>;

pub async fn setup_common(
    block_interval: Option<f64>,
    filter: Option<EventFilter>,
) -> anyhow::Result<(
    AnvilInstance,
    RobustProvider<Ethereum>,
    TestCounter::TestCounterInstance<RootProvider>,
    EventFilter,
)> {
    let anvil = spawn_anvil(block_interval)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider.root().clone()).await?;

    let default_filter =
        EventFilter::new().contract_address(*contract.address()).event(CountIncreased::SIGNATURE);

    let filter = filter.unwrap_or(default_filter);

    Ok((anvil, provider, contract, filter))
}

pub async fn setup_live_scanner(
    block_interval: Option<f64>,
    filter: Option<EventFilter>,
    confirmations: u64,
) -> anyhow::Result<LiveScannerSetup<impl Provider<Ethereum> + Clone>> {
    let (anvil, provider, contract, filter) = setup_common(block_interval, filter).await?;

    let mut scanner =
        EventScannerBuilder::live().block_confirmations(confirmations).connect(provider.clone());

    let stream = scanner.subscribe(filter);

    Ok(ScannerSetup { provider, contract, scanner, stream, anvil })
}

pub async fn setup_sync_scanner(
    block_interval: Option<f64>,
    filter: Option<EventFilter>,
    from: impl Into<BlockNumberOrTag>,
    confirmations: u64,
) -> anyhow::Result<SyncScannerSetup<impl Provider<Ethereum> + Clone>> {
    let (anvil, provider, contract, filter) = setup_common(block_interval, filter).await?;

    let mut scanner = EventScannerBuilder::sync()
        .from_block(from)
        .block_confirmations(confirmations)
        .connect(provider.clone());

    let stream = scanner.subscribe(filter);

    Ok(ScannerSetup { provider, contract, scanner, stream, anvil })
}

pub async fn setup_sync_from_latest_scanner(
    block_interval: Option<f64>,
    filter: Option<EventFilter>,
    latest: usize,
    confirmations: u64,
) -> anyhow::Result<SyncFromLatestScannerSetup<impl Provider<Ethereum> + Clone>> {
    let (anvil, provider, contract, filter) = setup_common(block_interval, filter).await?;

    let mut scanner = EventScannerBuilder::sync()
        .from_latest(latest)
        .block_confirmations(confirmations)
        .connect(provider.clone());

    let stream = scanner.subscribe(filter);

    Ok(ScannerSetup { provider, contract, scanner, stream, anvil })
}

pub async fn setup_historic_scanner(
    block_interval: Option<f64>,
    filter: Option<EventFilter>,
    from: BlockNumberOrTag,
    to: BlockNumberOrTag,
) -> anyhow::Result<HistoricScannerSetup<impl Provider<Ethereum> + Clone>> {
    let (anvil, provider, contract, filter) = setup_common(block_interval, filter).await?;
    let mut scanner =
        EventScannerBuilder::historic().from_block(from).to_block(to).connect(provider.clone());

    let stream = scanner.subscribe(filter);

    Ok(ScannerSetup { provider, contract, scanner, stream, anvil })
}

pub async fn setup_latest_scanner(
    block_interval: Option<f64>,
    filter: Option<EventFilter>,
    count: usize,
    from: Option<BlockNumberOrTag>,
    to: Option<BlockNumberOrTag>,
) -> anyhow::Result<LatestScannerSetup<impl Provider<Ethereum> + Clone>> {
    let (anvil, provider, contract, filter) = setup_common(block_interval, filter).await?;
    let mut builder = EventScannerBuilder::latest(count);
    if let Some(f) = from {
        builder = builder.from_block(f);
    }
    if let Some(t) = to {
        builder = builder.to_block(t);
    }

    let mut scanner = builder.connect(provider.clone());

    let stream = scanner.subscribe(filter);

    Ok(ScannerSetup { provider, contract, scanner, stream, anvil })
}
