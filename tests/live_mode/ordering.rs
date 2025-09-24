use std::{sync::Arc, time::Duration};

use crate::{
    common,
    mock_callbacks::{BlockOrderingCallback, EventOrderingCallback},
};
use alloy::{eips::BlockNumberOrTag, network::Ethereum, sol_types::SolEvent};
use common::{TestCounter, build_provider, deploy_counter, spawn_anvil};
use event_scanner::{event_scanner::EventScannerBuilder, event_filters::EventFilter};
use tokio::time::{sleep, timeout};

#[tokio::test]
async fn callback_occurs_in_order() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;

    let counts = Arc::new(tokio::sync::Mutex::new(Vec::<u64>::new()));
    let callback = Arc::new(EventOrderingCallback { counts: Arc::clone(&counts) });

    let filter = EventFilter {
        contract_address: Some(*contract.address()),
        event: Some(TestCounter::CountIncreased::SIGNATURE.to_owned()),
        callback,
    };
    let mut scanner = EventScannerBuilder::new()
        .with_event_filter(filter)
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;

    tokio::spawn(async move { scanner.start(BlockNumberOrTag::Latest, None).await });

    for _ in 0..5 {
        contract.increase().send().await?.watch().await?;
    }

    let expected: Vec<u64> = (1..=5).collect();
    let expected_clone = expected.clone();
    let counts_clone = Arc::clone(&counts);

    let event_counting = async move {
        while *counts_clone.lock().await != expected_clone {
            sleep(Duration::from_millis(100)).await;
        }
    };

    if timeout(Duration::from_secs(1), event_counting).await.is_err() {
        anyhow::bail!(
            "callback ordering mismatch counts, expected: {expected:?}: {:?}",
            *counts.lock().await
        );
    }

    Ok(())
}

#[tokio::test]
async fn blocks_and_events_arrive_in_order() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider.clone()).await?;

    let blocks = Arc::new(tokio::sync::Mutex::new(Vec::<u64>::new()));
    let callback = Arc::new(BlockOrderingCallback { blocks: Arc::clone(&blocks) });

    let filter = EventFilter {
        contract_address: Some(*contract.address()),
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
        let _pending = contract.increase().send().await?;
        sleep(Duration::from_millis(200)).await;
    }

    let blocks_clone = Arc::clone(&blocks);

    let event_counting = async move {
        while blocks_clone.lock().await.len() < expected_event_count {
            sleep(Duration::from_millis(100)).await;
        }
    };

    if timeout(Duration::from_secs(1), event_counting).await.is_err() {
        anyhow::bail!("expected {expected_event_count} events, got {}", blocks.lock().await.len());
    }

    let data = blocks.lock().await.clone();
    assert!(
        data.windows(2).all(|w| w[0] <= w[1]),
        "block numbers must be non-decreasing: {data:?}",
    );

    Ok(())
}
