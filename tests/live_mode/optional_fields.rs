use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::common::{TestCounter, build_provider, deploy_counter, spawn_anvil};
use alloy::{eips::BlockNumberOrTag, network::Ethereum, sol_types::SolEvent};
use event_scanner::{
    event_filter::EventFilter,
    event_scanner::{EventScanner, EventScannerMessage},
};
use tokio::time::timeout;
use tokio_stream::StreamExt;

#[tokio::test]
async fn track_all_events_from_contract() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();

    let mut client = EventScanner::new().connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;

    // Create filter that tracks ALL events from a specific contract (no event signature specified)
    let filter = EventFilter {
        contract_address: Some(contract_address),
        event: None, // Track all events from this contract
    };
    let expected_event_count = 5;

    let mut stream = client.create_event_stream(filter).take(expected_event_count);

    tokio::spawn(async move { client.start_scanner(BlockNumberOrTag::Latest, None).await });

    // Generate both increase and decrease events
    for _ in 0..expected_event_count {
        contract.increase().send().await?.watch().await?;
    }

    // Also generate some decrease events to ensure we're tracking all events
    contract.decrease().send().await?.watch().await?;

    let event_count = Arc::new(AtomicUsize::new(0));
    let event_count_clone = Arc::clone(&event_count);
    let event_counting = async move {
        while let Some(message) = stream.next().await {
            match message {
                EventScannerMessage::Data(logs) => {
                    event_count_clone.fetch_add(logs.len(), Ordering::SeqCst);
                }
                EventScannerMessage::Error(e) => {
                    panic!("panicked with error {e}");
                }
                EventScannerMessage::Status(_) => {}
            }
        }
    };

    _ = timeout(Duration::from_secs(2), event_counting).await;

    assert_eq!(event_count.load(Ordering::SeqCst), expected_event_count);

    Ok(())
}

#[tokio::test]
async fn track_all_events_in_block_range() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider.clone()).await?;

    // Create filter that tracks ALL events in block range (no contract address or event signature
    // specified)
    let filter = EventFilter {
        contract_address: None, // Track events from all contracts
        event: None,            // Track all event types
    };
    let expected_event_count = 3;

    let mut client = EventScanner::new().connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;

    let mut stream = client.create_event_stream(filter).take(expected_event_count);

    tokio::spawn(async move { client.start_scanner(BlockNumberOrTag::Latest, None).await });

    // Generate events from our contract
    for _ in 0..expected_event_count {
        contract.increase().send().await?.watch().await?;
    }

    let event_count = Arc::new(AtomicUsize::new(0));
    let event_count_clone = Arc::clone(&event_count);
    let event_counting = async move {
        while let Some(message) = stream.next().await {
            match message {
                EventScannerMessage::Data(logs) => {
                    event_count_clone.fetch_add(logs.len(), Ordering::SeqCst);
                }
                EventScannerMessage::Error(e) => {
                    panic!("panicked with error {e}");
                }
                EventScannerMessage::Status(_) => {}
            }
        }
    };

    _ = timeout(Duration::from_secs(2), event_counting).await;

    assert_eq!(event_count.load(Ordering::SeqCst), expected_event_count);

    Ok(())
}

#[tokio::test]
async fn mixed_optional_and_required_filters() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();

    // Filter for specific event from specific contract
    let specific_filter = EventFilter {
        contract_address: Some(contract_address),
        event: Some(TestCounter::CountIncreased::SIGNATURE.to_owned()),
    };
    let expected_specific_count = 2;

    // Filter for all events from all contracts
    let all_events_filter = EventFilter { contract_address: None, event: None };
    let expected_all_count = 3;

    let mut client = EventScanner::new().connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;

    let mut specific_stream =
        client.create_event_stream(specific_filter).take(expected_specific_count);
    let mut all_stream = client.create_event_stream(all_events_filter).take(expected_all_count);

    tokio::spawn(async move { client.start_scanner(BlockNumberOrTag::Latest, None).await });

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
        while let Some(message) = all_stream.next().await {
            match message {
                EventScannerMessage::Data(logs) => {
                    all_count_clone.fetch_add(logs.len(), Ordering::SeqCst);
                }
                EventScannerMessage::Error(e) => {
                    panic!("panicked with error {e}");
                }
                EventScannerMessage::Status(_) => {}
            }
        }
        while let Some(message) = specific_stream.next().await {
            match message {
                EventScannerMessage::Data(logs) => {
                    specific_count_clone.fetch_add(logs.len(), Ordering::SeqCst);
                }
                EventScannerMessage::Error(e) => {
                    panic!("panicked with error {e}");
                }
                EventScannerMessage::Status(_) => {}
            }
        }
    };

    _ = timeout(Duration::from_secs(3), event_counting).await;

    assert_eq!(specific_event_count.load(Ordering::SeqCst), expected_specific_count);
    assert_eq!(all_events_count.load(Ordering::SeqCst), expected_all_count);

    Ok(())
}
