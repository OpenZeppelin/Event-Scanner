use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::common::{TestCounter, setup_live_scanner};
use alloy::sol_types::SolEvent;
use event_scanner::{EventFilter, EventScannerMessage};
use tokio::time::timeout;
use tokio_stream::StreamExt;

#[tokio::test]
async fn track_all_events_from_contract() -> anyhow::Result<()> {
    let setup = setup_live_scanner(Some(0.1), None, 0).await?;
    let contract = setup.contract.clone();
    let contract_address = *contract.address();

    let mut scanner = setup.scanner;

    // Create filter that tracks ALL events from a specific contract (no event signature specified)
    let filter = EventFilter::new().contract_address(contract_address);
    let expected_event_count = 5;

    let mut stream = scanner.subscribe(filter).take(expected_event_count);

    tokio::spawn(async move { scanner.start().await });

    // Generate both increase and decrease events
    for _ in 0..expected_event_count {
        contract.increase().send().await?.watch().await?;
    }

    // Also generate some decrease events to ensure we're tracking all events
    contract.decrease().send().await?.watch().await?;

    let event_count = Arc::new(AtomicUsize::new(0));
    let event_count_clone = Arc::clone(&event_count);
    let event_counting = async move {
        while let Some(EventScannerMessage::Data(logs)) = stream.next().await {
            event_count_clone.fetch_add(logs.len(), Ordering::SeqCst);
        }
    };

    _ = timeout(Duration::from_secs(2), event_counting).await;

    assert_eq!(event_count.load(Ordering::SeqCst), expected_event_count);

    Ok(())
}

#[tokio::test]
async fn track_all_events_in_block_range() -> anyhow::Result<()> {
    let setup = setup_live_scanner(Some(0.1), None, 0).await?;
    let contract = setup.contract.clone();

    // Create filter that tracks ALL events in block range (no contract address or event signature
    // specified)
    let filter = EventFilter::new();
    let expected_event_count = 3;

    let mut scanner = setup.scanner;

    let mut stream = scanner.subscribe(filter).take(expected_event_count);

    tokio::spawn(async move { scanner.start().await });

    // Generate events from our contract
    for _ in 0..expected_event_count {
        contract.increase().send().await?.watch().await?;
    }

    let event_count = Arc::new(AtomicUsize::new(0));
    let event_count_clone = Arc::clone(&event_count);
    let event_counting = async move {
        while let Some(EventScannerMessage::Data(logs)) = stream.next().await {
            event_count_clone.fetch_add(logs.len(), Ordering::SeqCst);
        }
    };

    _ = timeout(Duration::from_secs(2), event_counting).await;

    assert_eq!(event_count.load(Ordering::SeqCst), expected_event_count);

    Ok(())
}

#[tokio::test]
async fn mixed_optional_and_required_filters() -> anyhow::Result<()> {
    let setup = setup_live_scanner(Some(0.1), None, 0).await?;
    let contract = setup.contract.clone();
    let contract_address = *contract.address();

    // Filter for specific event from specific contract
    let specific_filter = EventFilter::new()
        .contract_address(contract_address)
        .event(TestCounter::CountIncreased::SIGNATURE);
    let expected_specific_count = 2;

    // Filter for all events from all contracts
    let all_events_filter = EventFilter::new();
    let expected_all_count = 3;

    let mut scanner = setup.scanner;

    let mut specific_stream =
        scanner.subscribe(specific_filter).take(expected_specific_count);
    let mut all_stream = scanner.subscribe(all_events_filter).take(expected_all_count);

    tokio::spawn(async move { scanner.start().await });

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

    let specific_event_count = Arc::new(AtomicUsize::new(0));
    let all_events_count = Arc::new(AtomicUsize::new(0));
    let specific_count_clone = Arc::clone(&specific_event_count);
    let all_count_clone = Arc::clone(&all_events_count);

    let event_counting = async move {
        while let Some(EventScannerMessage::Data(logs)) = all_stream.next().await {
            all_count_clone.fetch_add(logs.len(), Ordering::SeqCst);
        }
        while let Some(EventScannerMessage::Data(logs)) = specific_stream.next().await {
            specific_count_clone.fetch_add(logs.len(), Ordering::SeqCst);
        }
    };

    _ = timeout(Duration::from_secs(3), event_counting).await;

    assert_eq!(specific_event_count.load(Ordering::SeqCst), expected_specific_count);
    assert_eq!(all_events_count.load(Ordering::SeqCst), expected_all_count);

    Ok(())
}
