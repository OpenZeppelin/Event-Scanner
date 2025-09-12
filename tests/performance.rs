use std::{sync::{Arc, atomic::{AtomicUsize, Ordering}}, time::Duration};

mod common;
use alloy::{network::Ethereum, providers::WsConnect, sol_types::SolEvent};
use common::{TestCounter, EventCounter, build_provider, deploy_counter, spawn_anvil};
use event_scanner::{types::EventFilter, event_scanner::EventScannerBuilder};
use tokio::time::sleep;

#[tokio::test]
async fn performance_high_event_volume_no_loss() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;

    let count = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(EventCounter { count: count.clone() });
    let filter = EventFilter { contract_address: *contract.address(), event: TestCounter::CountIncreased::SIGNATURE.to_owned(), callback };

    let builder = EventScannerBuilder::<Ethereum>::new().with_event_filter(filter);
    let mut scanner = builder.connect_ws(WsConnect::new(anvil.ws_endpoint_url())).await?;
    let scanner_handle = tokio::spawn(async move { scanner.start().await });

    for _ in 0..100 {
        let _ = contract.increase().send().await?.get_receipt().await?;
    }

    sleep(Duration::from_millis(800)).await;
    scanner_handle.abort();

    assert_eq!(count.load(Ordering::SeqCst), 100);
    Ok(())
}

