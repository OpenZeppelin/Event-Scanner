use alloy::{
    network::Ethereum,
    primitives::FixedBytes,
    providers::{Provider, RootProvider},
    rpc::types::anvil::{ReorgOptions, TransactionData},
};
use anyhow::Ok;
use std::{sync::Arc, time::Duration};
use tokio_stream::StreamExt;

use tokio::{sync::Mutex, time::timeout};

use crate::common::{TestCounter, TestSetup, setup_scanner};
use alloy::{eips::BlockNumberOrTag, providers::ext::AnvilApi};
use event_scanner::{event_scanner::EventScannerMessage, types::ScannerStatus};

async fn reorg_with_new_txs<P>(
    provider: RootProvider,
    contract: TestCounter::TestCounterInstance<Arc<P>>,
    num_initial_events: u64,
    num_new_events: u64,
    reorg_depth: u64,
    same_block: bool,
) -> anyhow::Result<Vec<FixedBytes<32>>>
where
    P: alloy::providers::Provider<Ethereum> + Clone,
{
    let mut event_tx_hashes = vec![];

    for _ in 0..num_initial_events {
        let receipt = contract.increase().send().await.unwrap().get_receipt().await.unwrap();
        event_tx_hashes.push(receipt.transaction_hash);
    }

    let mut tx_hashes = vec![];
    for i in 0..num_new_events {
        let tx = contract.increase().into_transaction_request();
        tx_hashes.push((TransactionData::JSON(tx), if same_block { 0 } else { i }));
    }
    let pre_reorg_block = provider.get_block_by_number(BlockNumberOrTag::Latest).await?.unwrap();

    let reorg_options = ReorgOptions { depth: reorg_depth, tx_block_pairs: tx_hashes };

    provider.anvil_reorg(reorg_options).await.unwrap();

    let post_reorg_block = provider
        .get_block_by_number(BlockNumberOrTag::Number(pre_reorg_block.number()))
        .full()
        .await?
        .unwrap();

    // sanity checks, block numbers stays the same but hash changes
    assert_eq!(post_reorg_block.header.number, pre_reorg_block.header.number);
    assert_ne!(post_reorg_block.header.hash, pre_reorg_block.header.hash);

    if same_block {
        // only check block we inserted events in
        let new_block = provider
            .get_block_by_number(BlockNumberOrTag::Number(
                post_reorg_block.header.number - reorg_depth + 1,
            ))
            .await?
            .unwrap();
        assert_eq!(new_block.transactions.len() as u64, num_new_events);

        for tx_hash in new_block.transactions.hashes() {
            event_tx_hashes.push(tx_hash);
        }
    } else {
        for i in 0..num_new_events {
            // check subquent blocks for events
            let new_block = provider
                .get_block_by_number(BlockNumberOrTag::Number(
                    post_reorg_block.header.number - reorg_depth + 1 + i,
                ))
                .await?
                .unwrap();
            assert_eq!(new_block.transactions.len() as u64, 1);

            for tx_hash in new_block.transactions.hashes() {
                event_tx_hashes.push(tx_hash);
            }
        }
    }

    Ok(event_tx_hashes)
}

#[tokio::test]
async fn reorg_rescans_events_within_same_block() -> anyhow::Result<()> {
    let TestSetup { provider, contract, client, mut stream, anvil: _anvil } =
        setup_scanner(Option::Some(0.1), Option::None, Option::None).await?;

    tokio::spawn(async move { client.start_scanner(BlockNumberOrTag::Latest, None).await });

    let num_initial_events = 5;
    let num_new_events = 3;
    let reorg_depth = 5;
    let same_block = true;

    let expected_event_tx_hashes = reorg_with_new_txs(
        provider,
        contract,
        num_initial_events,
        num_new_events,
        reorg_depth,
        same_block,
    )
    .await?;

    let event_block_count = Arc::new(Mutex::new(Vec::new()));
    let event_block_count_clone = Arc::clone(&event_block_count);

    let reorg_detected = Arc::new(Mutex::new(false));
    let reorg_detected_clone = reorg_detected.clone();

    let event_counting = async move {
        while let Some(message) = stream.next().await {
            match message {
                EventScannerMessage::Data(logs) => {
                    let mut guard = event_block_count_clone.lock().await;
                    for log in logs {
                        if let Some(n) = log.transaction_hash {
                            guard.push(n);
                        }
                    }
                }
                EventScannerMessage::Error(e) => {
                    panic!("panic with error {e}");
                }
                EventScannerMessage::Status(status) => {
                    if matches!(status, ScannerStatus::ReorgDetected) {
                        *reorg_detected_clone.lock().await = true;
                    }
                }
            }
        }
    };

    let _ = timeout(Duration::from_secs(5), event_counting).await;

    let final_blocks: Vec<_> = event_block_count.lock().await.clone();
    assert_eq!(final_blocks.len() as u64, num_initial_events + num_new_events);
    assert_eq!(final_blocks, expected_event_tx_hashes);
    assert!(*reorg_detected.lock().await);

    Ok(())
}

#[tokio::test]
async fn reorg_rescans_events_with_ascending_blocks() -> anyhow::Result<()> {
    let TestSetup { provider, contract, client, mut stream, anvil: _anvil } =
        setup_scanner(Option::Some(0.1), Option::None, Option::None).await?;

    tokio::spawn(async move { client.start_scanner(BlockNumberOrTag::Latest, None).await });

    let num_initial_events = 5;

    let reorg_depth = 5;
    let num_new_events = 3;
    // add events in ascending blocks from reorg point
    let same_block = false;

    let expected_event_tx_hashes = reorg_with_new_txs(
        provider,
        contract,
        num_initial_events,
        num_new_events,
        reorg_depth,
        same_block,
    )
    .await?;

    let event_block_count = Arc::new(Mutex::new(Vec::new()));
    let event_block_count_clone = Arc::clone(&event_block_count);

    let reorg_detected = Arc::new(Mutex::new(false));
    let reorg_detected_clone = reorg_detected.clone();

    let event_counting = async move {
        while let Some(message) = stream.next().await {
            match message {
                EventScannerMessage::Data(logs) => {
                    let mut guard = event_block_count_clone.lock().await;
                    for log in logs {
                        if let Some(n) = log.transaction_hash {
                            guard.push(n);
                        }
                    }
                }
                EventScannerMessage::Error(e) => {
                    panic!("panic with error {e}");
                }
                EventScannerMessage::Status(status) => {
                    if matches!(status, ScannerStatus::ReorgDetected) {
                        *reorg_detected_clone.lock().await = true;
                    }
                }
            }
        }
    };

    let _ = timeout(Duration::from_secs(10), event_counting).await;

    let final_blocks: Vec<_> = event_block_count.lock().await.clone();
    assert_eq!(final_blocks.len() as u64, num_initial_events + num_new_events);
    assert_eq!(final_blocks, expected_event_tx_hashes);
    assert!(*reorg_detected.lock().await);

    Ok(())
}

#[tokio::test]
async fn reorg_depth_one() -> anyhow::Result<()> {
    let TestSetup { provider, contract, client, mut stream, anvil: _anvil } =
        setup_scanner(Option::Some(0.1), Option::None, Option::None).await?;

    tokio::spawn(async move { client.start_scanner(BlockNumberOrTag::Latest, None).await });

    let num_initial_events = 4;

    let reorg_depth = 1;
    let num_new_events = 1;
    let same_block = true;

    let expected_event_tx_hashes = reorg_with_new_txs(
        provider,
        contract,
        num_initial_events,
        num_new_events,
        reorg_depth,
        same_block,
    )
    .await?;

    let event_block_count = Arc::new(Mutex::new(Vec::new()));
    let event_block_count_clone = Arc::clone(&event_block_count);

    let reorg_detected = Arc::new(Mutex::new(false));
    let reorg_detected_clone = reorg_detected.clone();

    let event_counting = async move {
        while let Some(message) = stream.next().await {
            match message {
                EventScannerMessage::Data(logs) => {
                    let mut guard = event_block_count_clone.lock().await;
                    for log in logs {
                        if let Some(n) = log.transaction_hash {
                            guard.push(n);
                        }
                    }
                }
                EventScannerMessage::Error(e) => {
                    panic!("panic with error {e}");
                }
                EventScannerMessage::Status(info) => {
                    if matches!(info, ScannerStatus::ReorgDetected) {
                        *reorg_detected_clone.lock().await = true;
                    }
                }
            }
        }
    };

    _ = timeout(Duration::from_secs(5), event_counting).await;

    let final_blocks: Vec<_> = event_block_count.lock().await.clone();
    assert_eq!(final_blocks.len() as u64, num_initial_events + num_new_events);
    assert_eq!(final_blocks, expected_event_tx_hashes);
    assert!(*reorg_detected.lock().await);

    Ok(())
}

#[tokio::test]
async fn reorg_depth_two() -> anyhow::Result<()> {
    let TestSetup { provider, contract, client, mut stream, anvil: _anvil } =
        setup_scanner(Option::Some(0.1), Option::None, Option::None).await?;

    tokio::spawn(async move { client.start_scanner(BlockNumberOrTag::Latest, None).await });

    let num_initial_events = 4;

    let num_new_events = 1;
    let reorg_depth = 2;

    let same_block = true;
    let expected_event_tx_hashes = reorg_with_new_txs(
        provider,
        contract,
        num_initial_events,
        num_new_events,
        reorg_depth,
        same_block,
    )
    .await?;

    let event_block_count = Arc::new(Mutex::new(Vec::new()));
    let event_block_count_clone = Arc::clone(&event_block_count);

    let reorg_detected = Arc::new(Mutex::new(false));
    let reorg_detected_clone = reorg_detected.clone();

    let event_counting = async move {
        while let Some(message) = stream.next().await {
            match message {
                EventScannerMessage::Data(logs) => {
                    let mut guard = event_block_count_clone.lock().await;
                    for log in logs {
                        if let Some(n) = log.transaction_hash {
                            guard.push(n);
                        }
                    }
                }
                EventScannerMessage::Error(e) => {
                    panic!("panic with error {e}");
                }
                EventScannerMessage::Status(info) => {
                    if matches!(info, ScannerStatus::ReorgDetected) {
                        *reorg_detected_clone.lock().await = true;
                    }
                }
            }
        }
    };

    _ = timeout(Duration::from_secs(5), event_counting).await;

    let final_blocks: Vec<_> = event_block_count.lock().await.clone();
    assert_eq!(final_blocks.len() as u64, num_initial_events + num_new_events);
    assert_eq!(final_blocks, expected_event_tx_hashes);
    assert!(*reorg_detected.lock().await);

    Ok(())
}

#[tokio::test]
async fn block_confirmations_mitigate_reorgs() -> anyhow::Result<()> {
    // any reorg ≤ 5 should be invisible to consumers
    let block_confirmations = 5;
    let TestSetup { provider, contract, client, mut stream, anvil: _anvil } =
        setup_scanner(Option::Some(1.0), Option::None, Option::Some(block_confirmations)).await?;

    tokio::spawn(async move { client.start_scanner(BlockNumberOrTag::Latest, None).await });

    provider.anvil_mine(Some(10), None).await?;

    let num_initial_events = 4_u64;
    let num_new_events = 2_u64;
    // reorg depth is less than confirmations -> mitigated
    let reorg_depth = 2_u64;
    let same_block = true;

    let all_tx_hashes = reorg_with_new_txs(
        provider.clone(),
        contract,
        num_initial_events,
        num_new_events,
        reorg_depth,
        same_block,
    )
    .await?;

    provider.anvil_mine(Some(10), None).await?;

    let observed_tx_hashes = Arc::new(Mutex::new(Vec::new()));
    let observed_tx_hashes_clone = Arc::clone(&observed_tx_hashes);

    // With sufficient confirmations, a shallow reorg should be fully masked
    let reorg_detected = Arc::new(Mutex::new(false));
    let reorg_detected_clone = reorg_detected.clone();

    let event_counting = async move {
        while let Some(message) = stream.next().await {
            match message {
                EventScannerMessage::Data(logs) => {
                    let mut guard = observed_tx_hashes_clone.lock().await;
                    for log in logs {
                        if let Some(n) = log.transaction_hash {
                            guard.push(n);
                        }
                    }
                }
                EventScannerMessage::Error(e) => {
                    panic!("panic with error {e}");
                }
                EventScannerMessage::Status(info) => {
                    if matches!(info, ScannerStatus::ReorgDetected) {
                        *reorg_detected_clone.lock().await = true;
                    }
                }
            }
        }
    };

    _ = timeout(Duration::from_secs(10), event_counting).await;

    let final_hashes: Vec<_> = observed_tx_hashes.lock().await.clone();

    // Split tx hashes [initial_before_reorg | post_reorg]
    let (initial_before_reorg, post_reorg) =
        all_tx_hashes.split_at(num_initial_events.try_into().unwrap());

    // Keep only the confirmed portion of the pre-reorg events
    let kept_initial = &initial_before_reorg
        [..initial_before_reorg.len().saturating_sub(reorg_depth.try_into().unwrap())];

    // Keep all post-reorg events we injected
    let kept_post_reorg = &post_reorg[..num_new_events.try_into().unwrap()];

    // sanity checks
    assert_eq!(
        final_hashes.len(),
        kept_initial.len() + kept_post_reorg.len(),
        "expected count = confirmed pre-reorg + all post-reorg events",
    );

    assert!(final_hashes.starts_with(kept_initial), "prefix should be confirmed pre-reorg events",);
    assert!(
        final_hashes.ends_with(kept_post_reorg),
        "suffix should be post-reorg events on new chain",
    );

    // Full equality for completeness
    // let expected: Vec<_> = kept_initial.append(&mut kept_post_reorg);
    let mut expected = kept_initial.to_owned().clone();
    let mut post_reorg_clone = kept_post_reorg.to_owned().clone();
    expected.append(&mut post_reorg_clone);
    assert_eq!(final_hashes, expected);

    assert!(
        !*reorg_detected.lock().await,
        "reorg should be fully mitigated by confirmations (no status emitted)",
    );

    Ok(())
}
