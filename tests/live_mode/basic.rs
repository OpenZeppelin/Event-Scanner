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
use anyhow::Context;
use event_scanner::{
    event_scanner::EventScanner,
    event_scanner_ref::EventScanner as EventScannerRef,
    types::{EventFilter, EventFilterRef},
};
use tokio::time::{sleep, timeout};
use tokio_stream::StreamExt;

#[tokio::test]
async fn basic_single_event_scanning() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();

    let filter = EventFilterRef {
        contract_address,
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
    };

    let scanner = EventScannerRef::new().connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;

    let client = scanner.run()?;
    let (_, mut stream) = client
        .subscribe(filter)
        .await
        .with_context(|| format!("subscribe failed at {}:{}", file!(), line!()))?;

    let expected_event_count = 5;

    for _ in 0..expected_event_count {
        contract.increase().send().await?.watch().await?;
    }

    let event_count = Arc::new(AtomicUsize::new(0));

    let event_count_clone = Arc::clone(&event_count);

    tokio::spawn(async move {
        // The same number of blocks should be created as events
        while let Some(Ok(logs)) = stream.next().await {
            event_count_clone.fetch_add(logs.len(), Ordering::SeqCst);
        }
    });

    let event_count_clone = Arc::clone(&event_count);
    let event_counting = async move {
        while event_count_clone.load(Ordering::SeqCst) < expected_event_count {
            sleep(Duration::from_millis(100)).await;
        }
    };

    if timeout(Duration::from_secs(1), event_counting).await.is_err() {
        assert_eq!(event_count.load(Ordering::SeqCst), expected_event_count);
    }

    Ok(())
}

#[tokio::test]
async fn multiple_contracts_same_event_isolate_callbacks() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let a = deploy_counter(provider.clone()).await?;
    let b = deploy_counter(provider.clone()).await?;

    let a_filter = EventFilterRef {
        contract_address: *a.address(),
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
    };
    let b_filter = EventFilterRef {
        contract_address: *b.address(),
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
    };

    let scanner = EventScannerRef::new().connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;

    let client = scanner.run()?;

    // Subscribe to events from both A and B contracts
    let (_, mut a_stream) = client
        .subscribe(a_filter)
        .await
        .with_context(|| format!("subscribe(A) failed at {}:{}", file!(), line!()))?;
    let (_, mut b_stream) = client
        .subscribe(b_filter)
        .await
        .with_context(|| format!("subscribe(B) failed at {}:{}", file!(), line!()))?;

    let expected_events_a = 3;
    let expected_events_b = 2;

    // Start emitting events
    tokio::spawn(async move {
        for _ in 0..expected_events_a {
            a.increase()
                .send()
                .await
                .expect("should emit A")
                .watch()
                .await
                .expect("should confirm tx for A");
        }
    });

    tokio::spawn(async move {
        for _ in 0..expected_events_b {
            b.increase()
                .send()
                .await
                .expect("should emit B")
                .watch()
                .await
                .expect("should confirm tx for B");
        }
    });

    let a_count = Arc::new(AtomicUsize::new(0));
    let b_count = Arc::new(AtomicUsize::new(0));

    let a_count_clone = Arc::clone(&a_count);
    let b_count_clone = Arc::clone(&b_count);

    // Start processing events from both contracts
    let event_counting = async move {
        while a_count_clone.load(Ordering::SeqCst) != expected_events_a {
            let logs = a_stream.next().await.expect("should receive event").expect("should be Ok");
            a_count_clone.fetch_add(logs.len(), Ordering::SeqCst);
        }
        while b_count_clone.load(Ordering::SeqCst) != expected_events_b {
            let logs = b_stream.next().await.expect("should receive event").expect("should be Ok");
            b_count_clone.fetch_add(logs.len(), Ordering::SeqCst);
        }
    };

    if timeout(Duration::from_secs(1), event_counting).await.is_err() {
        assert_eq!(a_count.load(Ordering::SeqCst), expected_events_a);
        assert_eq!(b_count.load(Ordering::SeqCst), expected_events_b);
    }

    Ok(())
}

#[tokio::test]
async fn multiple_events_same_contract() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;
    let contract_address = *contract.address();

    let increase_count = Arc::new(AtomicUsize::new(0));
    let decrease_count = Arc::new(AtomicUsize::new(0));
    let increase_cb = Arc::new(BasicCounterCallback { count: Arc::clone(&increase_count) });
    let decrease_cb = Arc::new(BasicCounterCallback { count: Arc::clone(&decrease_count) });

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

    let scanner = EventScanner::new()
        .with_event_filters(vec![increase_filter, decrease_filter])
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;

    let client = scanner.run()?;
    tokio::spawn(async move {
        _ = client.subscribe(BlockNumberOrTag::Latest, None).await;
    });

    let expected_incr_events = 6;
    let expected_decr_events = 2;

    for _ in 0..expected_incr_events {
        contract.increase().send().await?.watch().await?;
    }

    contract.decrease().send().await?.watch().await?;
    contract.decrease().send().await?.watch().await?;

    let increase_count_clone = Arc::clone(&increase_count);
    let decrease_count_clone = Arc::clone(&decrease_count);
    let event_counting = async move {
        while increase_count_clone.load(Ordering::SeqCst) < expected_incr_events ||
            decrease_count_clone.load(Ordering::SeqCst) < expected_decr_events
        {
            sleep(Duration::from_millis(100)).await;
        }
    };

    if timeout(Duration::from_secs(2), event_counting).await.is_err() {
        assert_eq!(increase_count.load(Ordering::SeqCst), expected_incr_events);
        assert_eq!(decrease_count.load(Ordering::SeqCst), expected_decr_events);
    }

    Ok(())
}

#[tokio::test]
async fn signature_matching_ignores_irrelevant_events() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;

    let event_count = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(BasicCounterCallback { count: Arc::clone(&event_count) });

    // Subscribe to CountDecreased but only emit CountIncreased
    let filter = EventFilter {
        contract_address: *contract.address(),
        event: TestCounter::CountDecreased::SIGNATURE.to_owned(),
        callback,
    };

    let scanner = EventScanner::new()
        .with_event_filter(filter)
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;

    let client = scanner.run()?;
    tokio::spawn(async move {
        _ = client.subscribe(BlockNumberOrTag::Latest, None).await;
    });

    for _ in 0..3 {
        contract.increase().send().await?.watch().await?;
    }

    let event_count_clone = Arc::clone(&event_count);
    let event_counting = async move {
        while event_count_clone.load(Ordering::SeqCst) == 0 {
            sleep(Duration::from_millis(100)).await;
        }
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

    let event_count = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(BasicCounterCallback { count: Arc::clone(&event_count) });
    let filter = EventFilter {
        contract_address: *contract.address(),
        event: "invalid-sig".to_string(),
        callback,
    };

    let scanner = EventScanner::new()
        .with_event_filter(filter)
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;

    let client = scanner.run()?;
    tokio::spawn(async move {
        _ = client.subscribe(BlockNumberOrTag::Latest, None).await;
    });

    for _ in 0..3 {
        contract.increase().send().await?.watch().await?;
    }

    let event_count_clone = Arc::clone(&event_count);
    let event_counting = async move {
        while event_count_clone.load(Ordering::SeqCst) == 0 {
            sleep(Duration::from_millis(100)).await;
        }
    };

    if timeout(Duration::from_secs(1), event_counting).await.is_ok() {
        anyhow::bail!("scanner should have ignored all of the emitted events");
    }

    Ok(())
}
