use std::{sync::{Arc, atomic::{AtomicUsize, Ordering}}, time::Duration};

mod common;
use alloy::{network::Ethereum, providers::WsConnect, sol_types::SolEvent};
use common::{TestCounter, EventCounter, build_provider, deploy_counter, spawn_anvil};
use event_scanner::{types::EventFilter, event_scanner::EventScannerBuilder};
use tokio::time::sleep;

#[tokio::test]
async fn live_filters_multiple_events_same_contract() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;
    let contract_address = *contract.address();

    let increase_count = Arc::new(AtomicUsize::new(0));
    let decrease_count = Arc::new(AtomicUsize::new(0));
    let increase_cb = Arc::new(EventCounter { count: increase_count.clone() });
    let decrease_cb = Arc::new(EventCounter { count: decrease_count.clone() });

    let increase_filter = EventFilter { contract_address, event: TestCounter::CountIncreased::SIGNATURE.to_owned(), callback: increase_cb };
    let decrease_filter = EventFilter { contract_address, event: TestCounter::CountDecreased::SIGNATURE.to_owned(), callback: decrease_cb };

    let builder = EventScannerBuilder::<Ethereum>::new()
        .with_event_filter(increase_filter)
        .with_event_filter(decrease_filter);
    let mut scanner = builder.connect_ws(WsConnect::new(anvil.ws_endpoint_url())).await?;

    let scanner_handle = tokio::spawn(async move { scanner.start().await });

    for i in 0..6 {
        let _ = contract.increase().send().await?.get_receipt().await?;
        if i >= 4 { let _ = contract.decrease().send().await?.get_receipt().await?; }
    }

    sleep(Duration::from_millis(200)).await;
    scanner_handle.abort();

    assert_eq!(increase_count.load(Ordering::SeqCst), 6);
    assert_eq!(decrease_count.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn live_filters_signature_matching_ignores_irrelevant() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;

    let count = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(EventCounter { count: count.clone() });

    // Subscribe to CountDecreased but only emit CountIncreased
    let filter = EventFilter { contract_address: *contract.address(), event: TestCounter::CountDecreased::SIGNATURE.to_owned(), callback };

    let builder = EventScannerBuilder::<Ethereum>::new().with_event_filter(filter);
    let mut scanner = builder.connect_ws(WsConnect::new(anvil.ws_endpoint_url())).await?;
    let scanner_handle = tokio::spawn(async move { scanner.start().await });

    for _ in 0..3 { let _ = contract.increase().send().await?.get_receipt().await?; }

    sleep(Duration::from_millis(300)).await;
    scanner_handle.abort();

    assert_eq!(count.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn live_filters_malformed_signature_graceful() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;

    let count = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(EventCounter { count: count.clone() });
    let filter = EventFilter { contract_address: *contract.address(), event: "definitely-not-a-signature".to_string(), callback };

    let builder = EventScannerBuilder::<Ethereum>::new().with_event_filter(filter);
    let mut scanner = builder.connect_ws(WsConnect::new(anvil.ws_endpoint_url())).await?;
    let scanner_handle = tokio::spawn(async move { scanner.start().await });

    for _ in 0..3 { let _ = contract.increase().send().await?.get_receipt().await?; }

    sleep(Duration::from_millis(300)).await;
    scanner_handle.abort();

    assert_eq!(count.load(Ordering::SeqCst), 0);
    Ok(())
}

