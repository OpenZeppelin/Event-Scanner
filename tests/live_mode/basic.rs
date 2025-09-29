use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::common::{TestCounter, build_provider, deploy_counter, spawn_anvil};
use alloy::{eips::BlockNumberOrTag, network::Ethereum, rpc::types::Log, sol_types::SolEvent};
use event_scanner::{
    event_filter::EventFilter,
    event_scanner::{EventScanner, EventScannerError, EventScannerMessage},
};
use tokio::time::timeout;
use tokio_stream::{StreamExt, wrappers::ReceiverStream};

#[tokio::test]
async fn basic_single_event_scanning() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();

    let filter = EventFilter {
        contract_address: Some(contract_address),
        event: Some(TestCounter::CountIncreased::SIGNATURE.to_owned()),
    };

    let expected_event_count = 5;

    let mut client = EventScanner::new().connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;
    let mut stream = client.create_event_stream(filter).take(expected_event_count);

    tokio::spawn(async move { client.start_scanner(BlockNumberOrTag::Latest, None).await });

    for _ in 0..expected_event_count {
        contract.increase().send().await?.watch().await?;
    }

    let event_count = Arc::new(AtomicUsize::new(0));
    let event_count_clone = Arc::clone(&event_count);
    let event_counting = async move {
        let mut expected_new_count = 1;
        while let Some(message) = stream.next().await {
            match message {
                EventScannerMessage::Logs(logs) => {
                    event_count_clone.fetch_add(logs.len(), Ordering::SeqCst);

                    for log in logs {
                        let TestCounter::CountIncreased { newCount } = log.log_decode().unwrap().inner.data;
                        assert_eq!(newCount, expected_new_count);
                        expected_new_count += 1;
                    }
                }
                EventScannerMessage::Error(e) => {
                    panic!("Received error: {}", e);
                }
                EventScannerMessage::Info(_) => {
                    // Handle info if needed
                }
            }
        }
    };

    _ = timeout(Duration::from_secs(1), event_counting).await;

    assert_eq!(event_count.load(Ordering::SeqCst), expected_event_count);

    Ok(())
}

#[tokio::test]
async fn multiple_contracts_same_event_isolate_callbacks() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let a = deploy_counter(provider.clone()).await?;
    let b = deploy_counter(provider.clone()).await?;

    let a_filter = EventFilter {
        contract_address: Some(*a.address()),
        event: Some(TestCounter::CountIncreased::SIGNATURE.to_owned()),
    };
    let b_filter = EventFilter {
        contract_address: Some(*b.address()),
        event: Some(TestCounter::CountIncreased::SIGNATURE.to_owned()),
    };
    let expected_events_a = 3;
    let expected_events_b = 2;

    let mut client = EventScanner::new().connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;

    let a_stream = client.create_event_stream(a_filter);
    let b_stream = client.create_event_stream(b_filter);

    tokio::spawn(async move { client.start_scanner(BlockNumberOrTag::Latest, None).await });

    for _ in 0..expected_events_a {
        a.increase().send().await?.watch().await?;
    }

    for _ in 0..expected_events_b {
        b.increase().send().await?.watch().await?;
    }

    let make_assertion =
        async |stream: ReceiverStream<EventScannerMessage>,
               expected_events| {
            let mut stream = stream.take(expected_events);

            let count = Arc::new(AtomicUsize::new(0));
            let count_clone = Arc::clone(&count);

            let event_counting = async move {
                let mut expected_new_count = 1;
                while let Some(message) = stream.next().await {
                    match message {
                        EventScannerMessage::Logs(logs) => {
                            count_clone.fetch_add(logs.len(), Ordering::SeqCst);

                            for log in logs {
                                let TestCounter::CountIncreased { newCount } =
                                    log.log_decode().unwrap().inner.data;
                                assert_eq!(newCount, expected_new_count);
                                expected_new_count += 1;
                            }
                        }
                        EventScannerMessage::Error(e) => {
                            panic!("Received error: {}", e);
                        }
                        EventScannerMessage::Info(_) => {
                            // Handle info if needed
                        }
                    }
                }
            };

            _ = timeout(Duration::from_secs(1), event_counting).await;
            assert_eq!(count.load(Ordering::SeqCst), expected_events);
        };

    make_assertion(a_stream, expected_events_a).await;
    make_assertion(b_stream, expected_events_b).await;

    Ok(())
}

#[tokio::test]
async fn multiple_events_same_contract() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;
    let contract_address = *contract.address();

    let increase_filter = EventFilter {
        contract_address: Some(contract_address),
        event: Some(TestCounter::CountIncreased::SIGNATURE.to_owned()),
    };
    let decrease_filter = EventFilter {
        contract_address: Some(contract_address),
        event: Some(TestCounter::CountDecreased::SIGNATURE.to_owned()),
    };

    let expected_incr_events = 6;
    let expected_decr_events = 2;

    let mut client = EventScanner::new().connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;

    let mut incr_stream = client.create_event_stream(increase_filter).take(expected_incr_events);
    let mut decr_stream = client.create_event_stream(decrease_filter).take(expected_decr_events);

    tokio::spawn(async move { client.start_scanner(BlockNumberOrTag::Latest, None).await });

    for _ in 0..expected_incr_events {
        contract.increase().send().await?.watch().await?;
    }

    contract.decrease().send().await?.watch().await?;
    contract.decrease().send().await?.watch().await?;

    let incr_count = Arc::new(AtomicUsize::new(0));
    let decr_count = Arc::new(AtomicUsize::new(0));
    let incr_count_clone = Arc::clone(&incr_count);
    let decr_count_clone = Arc::clone(&decr_count);

    let event_counting = async move {
        let mut expected_new_count = 0;

        // process CountIncreased
        while let Some(message) = incr_stream.next().await {
            match message {
                EventScannerMessage::Logs(logs) => {
                    incr_count_clone.fetch_add(logs.len(), Ordering::SeqCst);

                    for log in logs {
                        expected_new_count += 1;
                        let TestCounter::CountIncreased { newCount } = log.log_decode().unwrap().inner.data;
                        assert_eq!(newCount, expected_new_count);
                    }
                }
                EventScannerMessage::Error(e) => {
                    panic!("Received error: {}", e);
                }
                EventScannerMessage::Info(_) => {
                    // Handle info if needed
                }
            }
        }

        expected_new_count -= 1;

        // process CountDecreased
        while let Some(message) = decr_stream.next().await {
            match message {
                EventScannerMessage::Logs(logs) => {
                    decr_count_clone.fetch_add(logs.len(), Ordering::SeqCst);

                    for log in logs {
                        let TestCounter::CountDecreased { newCount } = log.log_decode().unwrap().inner.data;
                        assert_eq!(newCount, expected_new_count);
                        expected_new_count -= 1;
                    }
                }
                EventScannerMessage::Error(e) => {
                    panic!("Received error: {}", e);
                }
                EventScannerMessage::Info(_) => {
                    // Handle info if needed
                }
            }
        }
    };

    _ = timeout(Duration::from_secs(2), event_counting).await;

    assert_eq!(incr_count.load(Ordering::SeqCst), expected_incr_events);
    assert_eq!(decr_count.load(Ordering::SeqCst), expected_decr_events);

    Ok(())
}

#[tokio::test]
async fn signature_matching_ignores_irrelevant_events() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;

    // Subscribe to CountDecreased but only emit CountIncreased
    let filter = EventFilter {
        contract_address: Some(*contract.address()),
        event: Some(TestCounter::CountDecreased::SIGNATURE.to_owned()),
    };
    let num_of_events = 3;

    let mut client = EventScanner::new().connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;

    let mut stream = client.create_event_stream(filter).take(num_of_events);

    tokio::spawn(async move { client.start_scanner(BlockNumberOrTag::Latest, None).await });

    for _ in 0..num_of_events {
        contract.increase().send().await?.watch().await?;
    }

    let event_counting = async move {
        _ = stream.next().await;
    };

    if timeout(Duration::from_secs(1), event_counting).await.is_ok() {
        anyhow::bail!("scanner should have ignored all of the emitted events");
    }

    Ok(())
}

#[tokio::test]
async fn live_filters_malformed_signature_graceful() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;

    let filter = EventFilter {
        contract_address: Some(*contract.address()),
        event: Some("invalid-sig".to_string()),
    };
    let num_of_events = 3;

    let mut client = EventScanner::new().connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;

    let mut stream = client.create_event_stream(filter).take(num_of_events);

    tokio::spawn(async move { client.start_scanner(BlockNumberOrTag::Latest, None).await });

    for _ in 0..num_of_events {
        contract.increase().send().await?.watch().await?;
    }

    let event_counting = async move {
        _ = stream.next().await;
    };

    if timeout(Duration::from_secs(1), event_counting).await.is_ok() {
        anyhow::bail!("scanner should have ignored all of the emitted events");
    }

    Ok(())
}
