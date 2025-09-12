use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::common::{EventCounter, TestCounter, build_provider, deploy_counter, spawn_anvil};
use alloy::{network::Ethereum, providers::WsConnect, sol_types::SolEvent};
use event_scanner::{event_scanner::EventScannerBuilder, types::EventFilter};
use tokio::time::sleep;

#[tokio::test]
async fn basic_single_event_scanning() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();

    let event_count = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(EventCounter { count: event_count.clone() });

    let filter = EventFilter {
        contract_address,
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback,
    };

    let builder = EventScannerBuilder::<Ethereum>::new().with_event_filter(filter);
    let mut scanner = builder.connect_ws(WsConnect::new(anvil.ws_endpoint_url())).await?;

    let scanner_handle = tokio::spawn(async move { scanner.start().await });

    for _ in 0..5 {
        let _ = contract.increase().send().await?.get_receipt().await?;
    }

    sleep(Duration::from_millis(200)).await;
    scanner_handle.abort();

    assert_eq!(event_count.load(Ordering::SeqCst), 5);
    Ok(())
}

#[tokio::test]
async fn multiple_contracts_same_event_isolate_callbacks() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1)?;
    let provider = build_provider(&anvil).await?;
    let a = deploy_counter(provider.clone()).await?;
    let b = deploy_counter(provider.clone()).await?;

    let a_count = Arc::new(AtomicUsize::new(0));
    let b_count = Arc::new(AtomicUsize::new(0));
    let a_cb = Arc::new(EventCounter { count: a_count.clone() });
    let b_cb = Arc::new(EventCounter { count: b_count.clone() });

    let a_filter = EventFilter {
        contract_address: *a.address(),
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback: a_cb,
    };
    let b_filter = EventFilter {
        contract_address: *b.address(),
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback: b_cb,
    };

    let builder = EventScannerBuilder::<Ethereum>::new()
        .with_event_filter(a_filter)
        .with_event_filter(b_filter);
    let mut scanner = builder.connect_ws(WsConnect::new(anvil.ws_endpoint_url())).await?;
    let scanner_handle = tokio::spawn(async move { scanner.start().await });

    for _ in 0..3 {
        let _ = a.increase().send().await?.get_receipt().await?;
    }
    for _ in 0..2 {
        let _ = b.increase().send().await?.get_receipt().await?;
    }

    sleep(Duration::from_millis(300)).await;
    scanner_handle.abort();

    assert_eq!(a_count.load(Ordering::SeqCst), 3);
    assert_eq!(b_count.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn multiple_events_same_contract() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;
    let contract_address = *contract.address();

    let increase_count = Arc::new(AtomicUsize::new(0));
    let decrease_count = Arc::new(AtomicUsize::new(0));
    let increase_cb = Arc::new(EventCounter { count: increase_count.clone() });
    let decrease_cb = Arc::new(EventCounter { count: decrease_count.clone() });

    let increase_filter = EventFilter {
        contract_address,
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback: increase_cb,
    };
    let decrease_filter = EventFilter {
        contract_address,
        event: TestCounter::CountDecreased::SIGNATURE.to_owned(),
        callback: decrease_cb,
    };

    let builder = EventScannerBuilder::<Ethereum>::new()
        .with_event_filter(increase_filter)
        .with_event_filter(decrease_filter);
    let mut scanner = builder.connect_ws(WsConnect::new(anvil.ws_endpoint_url())).await?;

    let scanner_handle = tokio::spawn(async move { scanner.start().await });

    for i in 0..6 {
        let _ = contract.increase().send().await?.get_receipt().await?;
        if i >= 4 {
            let _ = contract.decrease().send().await?.get_receipt().await?;
        }
    }

    sleep(Duration::from_millis(200)).await;
    scanner_handle.abort();

    assert_eq!(increase_count.load(Ordering::SeqCst), 6);
    assert_eq!(decrease_count.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn signature_matching_ignores_irrelevant_events() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;

    let count = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(EventCounter { count: count.clone() });

    // Subscribe to CountDecreased but only emit CountIncreased
    let filter = EventFilter {
        contract_address: *contract.address(),
        event: TestCounter::CountDecreased::SIGNATURE.to_owned(),
        callback,
    };

    let builder = EventScannerBuilder::<Ethereum>::new().with_event_filter(filter);
    let mut scanner = builder.connect_ws(WsConnect::new(anvil.ws_endpoint_url())).await?;
    let scanner_handle = tokio::spawn(async move { scanner.start().await });

    for _ in 0..3 {
        let _ = contract.increase().send().await?.get_receipt().await?;
    }

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
    let filter = EventFilter {
        contract_address: *contract.address(),
        event: "invalid-sig".to_string(),
        callback,
    };

    let builder = EventScannerBuilder::<Ethereum>::new().with_event_filter(filter);
    let mut scanner = builder.connect_ws(WsConnect::new(anvil.ws_endpoint_url())).await?;
    let scanner_handle = tokio::spawn(async move { scanner.start().await });

    for _ in 0..3 {
        let _ = contract.increase().send().await?.get_receipt().await?;
    }

    sleep(Duration::from_millis(300)).await;
    scanner_handle.abort();

    assert_eq!(count.load(Ordering::SeqCst), 0);
    Ok(())
}
