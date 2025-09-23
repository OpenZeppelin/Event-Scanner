use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::common::{TestCounter, build_provider, deploy_counter, spawn_anvil};
use alloy::{eips::BlockNumberOrTag, network::Ethereum, rpc::types::Log, sol_types::SolEvent};
use event_scanner::{block_range_scanner, event_scanner::EventScanner, types::EventFilter};
use tokio::{sync::mpsc, time::timeout};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};

#[tokio::test]
async fn basic_single_event_scanning() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();

    let (sender, receiver) = mpsc::channel(100);
    let filter = EventFilter {
        contract_address,
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
        sender,
    };

    let mut scanner = EventScanner::new()
        .with_event_filter(filter)
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;

    tokio::spawn(async move { scanner.start(BlockNumberOrTag::Latest, None).await });

    let expected_event_count = 5;

    for _ in 0..expected_event_count {
        contract.increase().send().await?.watch().await?;
    }

    let mut stream = ReceiverStream::new(receiver).take(expected_event_count);

    let event_count = Arc::new(AtomicUsize::new(0));
    let event_count_clone = Arc::clone(&event_count);
    let event_counting = async move {
        let mut expected_new_count = 1;
        while let Some(Ok(logs)) = stream.next().await {
            event_count_clone.fetch_add(logs.len(), Ordering::SeqCst);

            for log in logs {
                let TestCounter::CountIncreased { newCount } = log.log_decode().unwrap().inner.data;
                assert_eq!(newCount, expected_new_count);
                expected_new_count += 1;
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

    let (a_sender, a_receiver) = mpsc::channel(100);
    let a_filter = EventFilter {
        contract_address: *a.address(),
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
        sender: a_sender,
    };
    let (b_sender, b_receiver) = mpsc::channel(100);
    let b_filter = EventFilter {
        contract_address: *b.address(),
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
        sender: b_sender,
    };

    let mut scanner = EventScanner::new()
        .with_event_filters(vec![a_filter, b_filter])
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;

    tokio::spawn(async move { scanner.start(BlockNumberOrTag::Latest, None).await });

    let expected_events_a = 3;
    let expected_events_b = 2;

    for _ in 0..expected_events_a {
        a.increase().send().await?.watch().await?;
    }

    for _ in 0..expected_events_b {
        b.increase().send().await?.watch().await?;
    }

    let make_assertion = async |receiver, expected_events| {
        let mut stream =
            ReceiverStream::<Result<Vec<Log>, block_range_scanner::Error>>::new(receiver)
                .take(expected_events);

        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = Arc::clone(&count);

        let event_counting = async move {
            let mut expected_new_count = 1;
            while let Some(Ok(logs)) = stream.next().await {
                count_clone.fetch_add(logs.len(), Ordering::SeqCst);

                for log in logs {
                    let TestCounter::CountIncreased { newCount } =
                        log.log_decode().unwrap().inner.data;
                    assert_eq!(newCount, expected_new_count);
                    expected_new_count += 1;
                }
            }
        };

        _ = timeout(Duration::from_secs(1), event_counting).await;
        assert_eq!(count.load(Ordering::SeqCst), expected_events);
    };

    make_assertion(a_receiver, expected_events_a).await;
    make_assertion(b_receiver, expected_events_b).await;

    Ok(())
}

#[tokio::test]
async fn multiple_events_same_contract() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;
    let contract_address = *contract.address();

    let (incr_sender, incr_receiver) = mpsc::channel(100);
    let increase_filter = EventFilter {
        contract_address,
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
        sender: incr_sender,
    };
    let (decr_sender, decr_receiver) = mpsc::channel(100);
    let decrease_filter = EventFilter {
        contract_address,
        event: TestCounter::CountDecreased::SIGNATURE.to_owned(),
        sender: decr_sender,
    };

    let mut scanner = EventScanner::new()
        .with_event_filters(vec![increase_filter, decrease_filter])
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;

    tokio::spawn(async move { scanner.start(BlockNumberOrTag::Latest, None).await });

    let expected_incr_events = 6;
    let expected_decr_events = 2;

    for _ in 0..expected_incr_events {
        contract.increase().send().await?.watch().await?;
    }

    contract.decrease().send().await?.watch().await?;
    contract.decrease().send().await?.watch().await?;

    let mut incr_stream =
        ReceiverStream::<Result<Vec<Log>, block_range_scanner::Error>>::new(incr_receiver)
            .take(expected_incr_events);
    let mut decr_stream =
        ReceiverStream::<Result<Vec<Log>, block_range_scanner::Error>>::new(decr_receiver)
            .take(expected_decr_events);

    let incr_count = Arc::new(AtomicUsize::new(0));
    let decr_count = Arc::new(AtomicUsize::new(0));
    let incr_count_clone = Arc::clone(&incr_count);
    let decr_count_clone = Arc::clone(&decr_count);

    let event_counting = async move {
        let mut expected_new_count = 0;

        // process CountIncreased
        while let Some(Ok(logs)) = incr_stream.next().await {
            incr_count_clone.fetch_add(logs.len(), Ordering::SeqCst);

            for log in logs {
                expected_new_count += 1;
                let TestCounter::CountIncreased { newCount } = log.log_decode().unwrap().inner.data;
                assert_eq!(newCount, expected_new_count);
            }
        }

        expected_new_count -= 1;

        // process CountDecreased
        while let Some(Ok(logs)) = decr_stream.next().await {
            decr_count_clone.fetch_add(logs.len(), Ordering::SeqCst);

            for log in logs {
                let TestCounter::CountDecreased { newCount } = log.log_decode().unwrap().inner.data;
                assert_eq!(newCount, expected_new_count);
                expected_new_count -= 1;
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
    let (sender, receiver) = mpsc::channel(100);
    let filter = EventFilter {
        contract_address: *contract.address(),
        event: TestCounter::CountDecreased::SIGNATURE.to_owned(),
        sender,
    };

    let mut scanner = EventScanner::new()
        .with_event_filter(filter)
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;

    tokio::spawn(async move { scanner.start(BlockNumberOrTag::Latest, None).await });

    let num_of_events = 3;
    for _ in 0..num_of_events {
        contract.increase().send().await?.watch().await?;
    }

    let mut stream = ReceiverStream::new(receiver).take(num_of_events);

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

    let (sender, receiver) = mpsc::channel(100);
    let filter = EventFilter {
        contract_address: *contract.address(),
        event: "invalid-sig".to_string(),
        sender,
    };

    let mut scanner = EventScanner::new()
        .with_event_filter(filter)
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;

    tokio::spawn(async move { scanner.start(BlockNumberOrTag::Latest, None).await });

    let num_of_events = 3;
    for _ in 0..num_of_events {
        contract.increase().send().await?.watch().await?;
    }

    let mut stream = ReceiverStream::new(receiver).take(num_of_events);

    let event_counting = async move {
        _ = stream.next().await;
    };

    if timeout(Duration::from_secs(1), event_counting).await.is_ok() {
        anyhow::bail!("scanner should have ignored all of the emitted events");
    }

    Ok(())
}
