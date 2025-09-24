use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::{
    common::{TestCounter, build_provider, deploy_counter, spawn_anvil},
    mock_callbacks::BasicCounterCallback,
};
use alloy::{eips::BlockNumberOrTag, network::Ethereum, sol_types::SolEvent};
use event_scanner::{event_filter::EventFilter, event_scanner::EventScannerBuilder};
use tokio::time::{sleep, timeout};

#[tokio::test]
async fn track_all_events_from_contract() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();

    let event_count = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(BasicCounterCallback { count: Arc::clone(&event_count) });

    // Create filter that tracks ALL events from a specific contract (no event signature specified)
    let filter = EventFilter {
        contract_address: Some(contract_address),
        event: None, // Track all events from this contract
        callback,
    };

    let mut scanner = EventScannerBuilder::new()
        .with_event_filter(filter)
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;

    tokio::spawn(async move { scanner.start(BlockNumberOrTag::Latest, None).await });

    let expected_event_count = 5;

    // Generate both increase and decrease events
    for _ in 0..expected_event_count {
        contract.increase().send().await?.watch().await?;
    }

    // Also generate some decrease events to ensure we're tracking all events
    contract.decrease().send().await?.watch().await?;

    let event_count_clone = Arc::clone(&event_count);
    let event_counting = async move {
        while event_count_clone.load(Ordering::SeqCst) < expected_event_count + 1 {
            sleep(Duration::from_millis(100)).await;
        }
    };

    if timeout(Duration::from_secs(2), event_counting).await.is_err() {
        anyhow::bail!(
            "Expected to receive {} events, got {}",
            expected_event_count + 1,
            event_count.load(Ordering::SeqCst)
        );
    }

    Ok(())
}

#[tokio::test]
async fn track_all_events_in_block_range() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider.clone()).await?;

    let event_count = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(BasicCounterCallback { count: Arc::clone(&event_count) });

    // Create filter that tracks ALL events in block range (no contract address or event signature
    // specified)
    let filter = EventFilter {
        contract_address: None, // Track events from all contracts
        event: None,            // Track all event types
        callback,
    };

    let mut scanner = EventScannerBuilder::new()
        .with_event_filter(filter)
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;

    tokio::spawn(async move { scanner.start(BlockNumberOrTag::Latest, None).await });

    let expected_event_count = 3;

    // Generate events from our contract
    for _ in 0..expected_event_count {
        contract.increase().send().await?.watch().await?;
    }

    let event_count_clone = Arc::clone(&event_count);
    let event_counting = async move {
        while event_count_clone.load(Ordering::SeqCst) < expected_event_count {
            sleep(Duration::from_millis(100)).await;
        }
    };

    if timeout(Duration::from_secs(2), event_counting).await.is_err() {
        anyhow::bail!(
            "Expected to receive {} events, got {}",
            expected_event_count,
            event_count.load(Ordering::SeqCst)
        );
    }

    Ok(())
}

#[tokio::test]
async fn mixed_optional_and_required_filters() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();

    let specific_event_count = Arc::new(AtomicUsize::new(0));
    let all_events_count = Arc::new(AtomicUsize::new(0));

    let specific_callback =
        Arc::new(BasicCounterCallback { count: Arc::clone(&specific_event_count) });
    let all_events_callback =
        Arc::new(BasicCounterCallback { count: Arc::clone(&all_events_count) });

    // Filter for specific event from specific contract
    let specific_filter = EventFilter {
        contract_address: Some(contract_address),
        event: Some(TestCounter::CountIncreased::SIGNATURE.to_owned()),
        callback: specific_callback,
    };

    // Filter for all events from all contracts
    let all_events_filter =
        EventFilter { contract_address: None, event: None, callback: all_events_callback };

    let mut scanner = EventScannerBuilder::new()
        .with_event_filters(vec![specific_filter, all_events_filter])
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;

    tokio::spawn(async move { scanner.start(BlockNumberOrTag::Latest, None).await });

    let expected_specific_count = 2;
    let expected_all_count = 3;

    // First increase the counter to have some balance
    for _ in 0..expected_all_count {
        contract.increase().send().await?.watch().await?;
    }

    // Generate specific events (CountIncreased)
    for _ in 0..expected_specific_count {
        contract.increase().send().await?.watch().await?;
    }

    // Generate additional events that should be caught by the all-events filter
    for _ in 0..expected_all_count {
        contract.decrease().send().await?.watch().await?;
    }

    let specific_count_clone = Arc::clone(&specific_event_count);
    let all_count_clone = Arc::clone(&all_events_count);

    let event_counting = async move {
        while specific_count_clone.load(Ordering::SeqCst) < expected_specific_count ||
            all_count_clone.load(Ordering::SeqCst) < expected_all_count
        {
            sleep(Duration::from_millis(100)).await;
        }
    };

    if timeout(Duration::from_secs(3), event_counting).await.is_err() {
        anyhow::bail!(
            "Expected specific: {}, got {}; Expected all: {}, got {}",
            expected_specific_count,
            specific_event_count.load(Ordering::SeqCst),
            expected_all_count,
            all_events_count.load(Ordering::SeqCst)
        );
    }

    Ok(())
}
