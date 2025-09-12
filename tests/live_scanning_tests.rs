use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use alloy::sol_types::SolEvent;
mod common;
use common::{LiveTestCounter, build_provider, deploy_counter, spawn_anvil};
use event_scanner::{EventFilter, ScannerBuilder};
use tokio::time::sleep;

use crate::common::{EventCounter, SlowProcessor};

#[tokio::test]
async fn test_live_scanning_basic() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();

    let event_count = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(EventCounter { count: event_count.clone() });

    let filter = EventFilter {
        contract_address,
        event: LiveTestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback,
    };

    let mut scanner =
        ScannerBuilder::new(anvil.ws_endpoint_url()).add_event_filter(filter).build().await?;

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
async fn test_live_scanning_multiple_events() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;
    let contract_address = *contract.address();

    let increase_count = Arc::new(AtomicUsize::new(0));
    let decrease_count = Arc::new(AtomicUsize::new(0));

    let increase_callback = Arc::new(EventCounter { count: increase_count.clone() });

    let decrease_callback = Arc::new(EventCounter { count: decrease_count.clone() });

    let increase_filter = EventFilter {
        contract_address,
        event: LiveTestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback: increase_callback,
    };

    let decrease_filter = EventFilter {
        contract_address,
        event: LiveTestCounter::CountDecreased::SIGNATURE.to_owned(),
        callback: decrease_callback,
    };

    let mut scanner = ScannerBuilder::new(anvil.ws_endpoint_url())
        .add_event_filter(increase_filter)
        .add_event_filter(decrease_filter)
        .build()
        .await?;

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
async fn test_live_scanning_with_slow_processor() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();

    let processed = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(SlowProcessor { delay_ms: 100, processed: processed.clone() });

    let filter = EventFilter {
        contract_address,
        event: LiveTestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback,
    };

    let mut scanner =
        ScannerBuilder::new(anvil.ws_endpoint_url()).add_event_filter(filter).build().await?;

    let scanner_handle = tokio::spawn(async move { scanner.start().await });

    for _ in 0..3 {
        let _ = contract.increase().send().await?.get_receipt().await?;
        // Less than processor delay
        sleep(Duration::from_millis(50)).await;
    }

    sleep(Duration::from_millis(200)).await;

    scanner_handle.abort();

    assert_eq!(processed.load(Ordering::SeqCst), 3);

    Ok(())
}
