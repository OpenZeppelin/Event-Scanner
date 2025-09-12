use std::time::Duration;

mod common;
use alloy::{network::Ethereum, providers::WsConnect, sol_types::SolEvent};
use common::{TestCounter, OrderingCapture, build_provider, deploy_counter, spawn_anvil};
use event_scanner::{types::EventFilter, event_scanner::EventScannerBuilder};
use tokio::time::sleep;

#[tokio::test]
async fn ordering_events_processed_in_block_order() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;

    let blocks = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::<u64>::new()));
    let callback = std::sync::Arc::new(OrderingCapture { blocks: blocks.clone() });

    let filter = EventFilter { contract_address: *contract.address(), event: TestCounter::CountIncreased::SIGNATURE.to_owned(), callback };
    let builder = EventScannerBuilder::<Ethereum>::new().with_event_filter(filter);
    let mut scanner = builder.connect_ws(WsConnect::new(anvil.ws_endpoint_url())).await?;
    let scanner_handle = tokio::spawn(async move { scanner.start().await });

    for _ in 0..5 { let _ = contract.increase().send().await?.get_receipt().await?; }

    sleep(Duration::from_millis(400)).await;
    scanner_handle.abort();

    let data = blocks.lock().await;
    let is_sorted = data.windows(2).all(|w| w[0] <= w[1]);
    assert!(is_sorted, "events must be processed in non-decreasing block order: {:?}", *data);
    Ok(())
}

