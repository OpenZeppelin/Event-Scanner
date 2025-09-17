use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use alloy::{eips::BlockNumberOrTag, network::Ethereum, sol_types::SolEvent};
use event_scanner::{event_scanner::EventScannerBuilder, types::EventFilter};
use tokio::time::{Duration, sleep};

use crate::{
    common::{TestCounter, build_provider, deploy_counter, spawn_anvil},
    mock_callbacks::BasicCounterCallback,
};

#[tokio::test]
async fn replays_historical_then_switches_to_live() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1.0)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();

    let historical_events = 3;
    let live_events = 2;

    let mut first_historical_block = None;

    for _ in 0..historical_events {
        let receipt = contract.increase().send().await?.get_receipt().await?;
        let block_number =
            receipt.block_number.expect("historical receipt should contain block number");

        if first_historical_block.is_none() {
            first_historical_block = Some(block_number);
        }
    }

    let start_block = first_historical_block.expect("at least one historical event recorded");

    let event_count = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(BasicCounterCallback { count: event_count.clone() });

    let filter = EventFilter {
        contract_address,
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback,
    };

    let mut builder = EventScannerBuilder::new();
    builder.with_event_filter(filter);
    let scanner = builder.connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;

    let scanner_handle = tokio::spawn(async move {
        let mut scanner = scanner;
        scanner.start(BlockNumberOrTag::Number(start_block), None).await
    });

    sleep(Duration::from_millis(200)).await;

    for _ in 0..live_events {
        let _ = contract.increase().send().await?.get_receipt().await?;
    }

    sleep(Duration::from_millis(500)).await;

    scanner_handle.abort();

    assert_eq!(event_count.load(Ordering::SeqCst), historical_events + live_events);
    Ok(())
}
