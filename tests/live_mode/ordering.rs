use std::{sync::Arc, time::Duration};

use crate::common;
use alloy::{network::Ethereum, providers::WsConnect, sol_types::SolEvent};
use common::{OrderingCapture, OrderingCaptureCount, TestCounter, build_provider, deploy_counter, spawn_anvil};
use event_scanner::{event_scanner::EventScannerBuilder, types::EventFilter};
use tokio::time::sleep;

#[tokio::test]
async fn callback_occurs_in_order() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;

    let counts = Arc::new(tokio::sync::Mutex::new(Vec::<u64>::new()));
    let callback = Arc::new(OrderingCaptureCount { counts: counts.clone() });

    let filter = EventFilter {
        contract_address: *contract.address(),
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback,
    };
    let builder = EventScannerBuilder::<Ethereum>::new().with_event_filter(filter);
    let mut scanner = builder.connect_ws(WsConnect::new(anvil.ws_endpoint_url())).await?;
    let scanner_handle = tokio::spawn(async move { scanner.start().await });

    for _ in 0..5 {
        let _ = contract.increase().send().await?.get_receipt().await?;
    }

    sleep(Duration::from_millis(400)).await;
    scanner_handle.abort();

    let data = counts.lock().await;
    let expected: Vec<u64> = (1..=5).collect();
    assert_eq!(*data, expected, "callback ordering mismatch counts: {:?}", *data);
    Ok(())
}

#[tokio::test]
async fn blocks_and_events_arrive_in_order() -> anyhow::Result<()> {
    // Mine a block every second and batch 5 txs per block
    let anvil = spawn_anvil(1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider.clone()).await?;

    // Capture block numbers in callback order
    let blocks = Arc::new(tokio::sync::Mutex::new(Vec::<u64>::new()));
    let callback = Arc::new(OrderingCapture { blocks: blocks.clone() });

    let filter = EventFilter {
        contract_address: *contract.address(),
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback,
    };
    let builder = EventScannerBuilder::<Ethereum>::new().with_event_filter(filter);
    let mut scanner = builder.connect_ws(WsConnect::new(anvil.ws_endpoint_url())).await?;
    let scanner_handle = tokio::spawn(async move { scanner.start().await });

    // 5 blocks, 5 events per block
    for _ in 0..5 {
        for _ in 0..5 {
            // Do not await receipt to allow batching into same timed block
            let _pending = contract.increase().send().await?;
        }
        // Wait for the next block to be mined (block_time = 1s)
        sleep(Duration::from_millis(1200)).await;
    }

    // Give scanner time to drain channel
    sleep(Duration::from_millis(800)).await;
    scanner_handle.abort();

    let data = blocks.lock().await.clone();
    // Expect 25 events captured
    assert_eq!(data.len(), 25, "expected 25 events, got {}: {:?}", data.len(), data);
    // Non-decreasing order overall
    assert!(data.windows(2).all(|w| w[0] <= w[1]), "block numbers must be non-decreasing: {:?}", data);

    // Group by consecutive block numbers and check structure: 5 groups, each with 5 events
    let mut unique_blocks: Vec<u64> = Vec::new();
    let mut group_counts: Vec<usize> = Vec::new();
    for b in data.iter().copied() {
        match unique_blocks.last() {
            Some(&last) if last == b => {
                if let Some(last_cnt) = group_counts.last_mut() { *last_cnt += 1; }
            }
            _ => {
                unique_blocks.push(b);
                group_counts.push(1);
            }
        }
    }

    assert_eq!(unique_blocks.len(), 5, "expected 5 mined blocks, got {:?}", unique_blocks);
    assert!(unique_blocks.windows(2).all(|w| w[0] < w[1]), "blocks must strictly increase: {:?}", unique_blocks);
    assert!(group_counts.iter().all(|&c| c == 5), "each block should have 5 events: {:?}", group_counts);

    Ok(())
}
