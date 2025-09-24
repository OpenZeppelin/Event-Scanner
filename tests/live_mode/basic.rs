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
use event_scanner::{event_scanner::EventScannerBuilder, event_filters::EventFilter};
use tokio::time::{sleep, timeout};

#[tokio::test]
async fn basic_single_event_scanning() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();

    let event_count = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(BasicCounterCallback { count: Arc::clone(&event_count) });

    let filter = EventFilter {
        contract_address: Some(contract_address),
        event: Some(TestCounter::CountIncreased::SIGNATURE.to_owned()),
        callback,
    };

    let mut scanner = EventScannerBuilder::new()
        .with_event_filter(filter)
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;

    tokio::spawn(async move { scanner.start(BlockNumberOrTag::Latest, None).await });

    let expected_event_count = 5;

    for _ in 0..expected_event_count {
        contract.increase().send().await?.watch().await?;
    }

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

    let a_count = Arc::new(AtomicUsize::new(0));
    let b_count = Arc::new(AtomicUsize::new(0));
    let a_cb = Arc::new(BasicCounterCallback { count: Arc::clone(&a_count) });
    let b_cb = Arc::new(BasicCounterCallback { count: Arc::clone(&b_count) });

    let a_filter = EventFilter {
        contract_address: Some(*a.address()),
        event: Some(TestCounter::CountIncreased::SIGNATURE.to_owned()),
        callback: a_cb,
    };
    let b_filter = EventFilter {
        contract_address: Some(*b.address()),
        event: Some(TestCounter::CountIncreased::SIGNATURE.to_owned()),
        callback: b_cb,
    };

    let mut scanner = EventScannerBuilder::new()
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

    let a_count_clone = Arc::clone(&a_count);
    let b_count_clone = Arc::clone(&b_count);
    let event_counting = async move {
        while a_count_clone.load(Ordering::SeqCst) < expected_events_a ||
            b_count_clone.load(Ordering::SeqCst) < expected_events_b
        {
            sleep(Duration::from_millis(100)).await;
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
        contract_address: Some(contract_address),
        event: Some(TestCounter::CountIncreased::SIGNATURE.to_owned()),
        callback: increase_cb,
    };
    let decrease_filter = EventFilter {
        contract_address: Some(contract_address),
        event: Some(TestCounter::CountDecreased::SIGNATURE.to_owned()),
        callback: decrease_cb,
    };

    let mut scanner = EventScannerBuilder::new()
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
        contract_address: Some(*contract.address()),
        event: Some(TestCounter::CountDecreased::SIGNATURE.to_owned()),
        callback,
    };

    let mut scanner = EventScannerBuilder::new()
        .with_event_filter(filter)
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;

    tokio::spawn(async move { scanner.start(BlockNumberOrTag::Latest, None).await });

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
        contract_address: Some(*contract.address()),
        event: Some("invalid-sig".to_string()),
        callback,
    };

    let mut scanner = EventScannerBuilder::new()
        .with_event_filter(filter)
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;

    tokio::spawn(async move { scanner.start(BlockNumberOrTag::Latest, None).await });

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
