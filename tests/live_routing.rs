use std::{sync::{Arc, atomic::{AtomicUsize, Ordering}}, time::Duration};

mod common;
use alloy::{network::Ethereum, providers::WsConnect, sol_types::SolEvent};
use common::{TestCounter, EventCounter, build_provider, deploy_counter, spawn_anvil};
use event_scanner::{types::EventFilter, event_scanner::EventScannerBuilder};
use tokio::time::sleep;

#[tokio::test]
async fn live_routing_multiple_contracts_isolated() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1)?;
    let provider = build_provider(&anvil).await?;
    let a = deploy_counter(provider.clone()).await?;
    let b = deploy_counter(provider.clone()).await?;

    let a_count = Arc::new(AtomicUsize::new(0));
    let b_count = Arc::new(AtomicUsize::new(0));
    let a_cb = Arc::new(EventCounter { count: a_count.clone() });
    let b_cb = Arc::new(EventCounter { count: b_count.clone() });

    let a_filter = EventFilter { contract_address: *a.address(), event: TestCounter::CountIncreased::SIGNATURE.to_owned(), callback: a_cb };
    let b_filter = EventFilter { contract_address: *b.address(), event: TestCounter::CountIncreased::SIGNATURE.to_owned(), callback: b_cb };

    let builder = EventScannerBuilder::<Ethereum>::new()
        .with_event_filter(a_filter)
        .with_event_filter(b_filter);
    let mut scanner = builder.connect_ws(WsConnect::new(anvil.ws_endpoint_url())).await?;
    let scanner_handle = tokio::spawn(async move { scanner.start().await });

    for _ in 0..3 { let _ = a.increase().send().await?.get_receipt().await?; }
    for _ in 0..2 { let _ = b.increase().send().await?.get_receipt().await?; }

    sleep(Duration::from_millis(300)).await;
    scanner_handle.abort();

    assert_eq!(a_count.load(Ordering::SeqCst), 3);
    assert_eq!(b_count.load(Ordering::SeqCst), 2);
    Ok(())
}

