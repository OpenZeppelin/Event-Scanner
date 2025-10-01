use alloy::{
    network::Ethereum,
    providers::{Provider, RootProvider},
    rpc::types::anvil::{ReorgOptions, TransactionData},
};
use anyhow::Ok;
use std::{cmp::min, sync::Arc, time::Duration};
use tokio_stream::StreamExt;

use tokio::{sync::Mutex, time::timeout};

use crate::common::{TestCounter, TestSetup, setup_scanner};
use alloy::{eips::BlockNumberOrTag, providers::ext::AnvilApi};
use event_scanner::{event_scanner::EventScannerMessage, types::ScannerStatus};

async fn reorg_with_new_txs<P>(
    provider: RootProvider,
    contract: TestCounter::TestCounterInstance<Arc<P>>,
    num_new_events: u64,
    reorg_depth: u64,
    mut return_array: Vec<u64>,
    same_block: bool,
) -> anyhow::Result<Vec<u64>>
where
    P: alloy::providers::Provider<Ethereum> + Clone,
{
    let mut tx_block_pairs = vec![];
    let latest_block = provider.get_block_by_number(BlockNumberOrTag::Latest).await?.unwrap();
    for i in 0..num_new_events {
        let tx = contract.increase().into_transaction_request();
        tx_block_pairs.push((TransactionData::JSON(tx), if same_block { 0 } else { i }));
        return_array
            .push(latest_block.header.number - reorg_depth + 1 + if same_block { 0 } else { i });
    }
    let reorg_options = ReorgOptions { depth: reorg_depth, tx_block_pairs };

    provider.anvil_reorg(reorg_options).await.unwrap();
    let new_latest_block =
        provider.get_block_by_number(BlockNumberOrTag::Latest).full().await?.unwrap();

    // sanity checks, block numbers stays the same but hash changes
    assert_eq!(new_latest_block.header.number, latest_block.header.number);
    assert_ne!(new_latest_block.header.hash, latest_block.header.hash);

    Ok(return_array)
}

#[tokio::test]
async fn reorg_rescans_events_within_same_block() -> anyhow::Result<()> {
    let TestSetup { provider, contract, client, mut stream, anvil: _anvil } =
        setup_scanner(Option::Some(0.1), Option::None, Option::None).await?;

    tokio::spawn(async move { client.start_scanner(BlockNumberOrTag::Latest, None).await });

    let mut expected_event_block_numbers = vec![];

    let initial_events = 5;

    for _ in 0..initial_events {
        let receipt = contract.increase().send().await.unwrap().get_receipt().await.unwrap();
        expected_event_block_numbers.push(receipt.block_number.unwrap());
    }

    let num_new_events = 3;
    let reorg_depth = 5;
    let same_block = true;

    expected_event_block_numbers = reorg_with_new_txs(
        provider,
        contract,
        num_new_events,
        reorg_depth,
        expected_event_block_numbers,
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
                        if let Some(n) = log.block_number {
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
    assert_eq!(final_blocks.len() as u64, initial_events + num_new_events);
    assert_eq!(final_blocks, expected_event_block_numbers);
    assert!(*reorg_detected.lock().await);

    Ok(())
}

#[tokio::test]
async fn reorg_rescans_events_with_ascending_blocks() -> anyhow::Result<()> {
    let TestSetup { provider, contract, client, mut stream, anvil: _anvil } =
        setup_scanner(Option::Some(0.1), Option::None, Option::None).await?;

    tokio::spawn(async move { client.start_scanner(BlockNumberOrTag::Latest, None).await });

    let mut expected_event_block_numbers = vec![];

    let initial_events = 5;
    for _ in 0..initial_events {
        let receipt = contract.increase().send().await.unwrap().get_receipt().await.unwrap();
        expected_event_block_numbers.push(receipt.block_number.unwrap());
    }

    let reorg_depth = 5;
    let num_new_events = 3;
    // add events in ascending blocks from reorg point
    let same_block = false;

    expected_event_block_numbers = reorg_with_new_txs(
        provider,
        contract,
        num_new_events,
        reorg_depth,
        expected_event_block_numbers,
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
                        if let Some(n) = log.block_number {
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
    assert_eq!(final_blocks.len() as u64, initial_events + num_new_events);
    assert_eq!(final_blocks, expected_event_block_numbers);
    assert!(*reorg_detected.lock().await);

    Ok(())
}

#[tokio::test]
async fn reorg_depth_one() -> anyhow::Result<()> {
    let TestSetup { provider, contract, client, mut stream, anvil: _anvil } =
        setup_scanner(Option::Some(0.1), Option::None, Option::None).await?;

    tokio::spawn(async move { client.start_scanner(BlockNumberOrTag::Latest, None).await });

    let mut expected_event_block_numbers = vec![];

    let initial_events = 4;
    for _ in 0..initial_events {
        let receipt = contract.increase().send().await.unwrap().get_receipt().await.unwrap();
        expected_event_block_numbers.push(receipt.block_number.unwrap());
    }

    let reorg_depth = 1;
    let num_new_events = 1;
    let same_block = true;

    expected_event_block_numbers = reorg_with_new_txs(
        provider,
        contract,
        num_new_events,
        reorg_depth,
        expected_event_block_numbers,
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
                        if let Some(n) = log.block_number {
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
    assert_eq!(final_blocks.len() as u64, initial_events + num_new_events);
    assert_eq!(final_blocks, expected_event_block_numbers);
    assert!(*reorg_detected.lock().await);

    Ok(())
}

#[tokio::test]
async fn reorg_depth_two() -> anyhow::Result<()> {
    let TestSetup { provider, contract, client, mut stream, anvil: _anvil } =
        setup_scanner(Option::Some(0.1), Option::None, Option::None).await?;

    tokio::spawn(async move { client.start_scanner(BlockNumberOrTag::Latest, None).await });

    let mut expected_event_block_numbers = vec![];

    let initial_events = 4;
    for _ in 0..initial_events {
        let receipt = contract.increase().send().await.unwrap().get_receipt().await.unwrap();
        expected_event_block_numbers.push(receipt.block_number.unwrap());
    }

    let num_new_events = 1;
    let reorg_depth = 2;

    let same_block = true;
    expected_event_block_numbers = reorg_with_new_txs(
        provider,
        contract,
        num_new_events,
        reorg_depth,
        expected_event_block_numbers,
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
                        if let Some(n) = log.block_number {
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
    assert_eq!(final_blocks.len() as u64, initial_events + num_new_events);
    assert_eq!(final_blocks, expected_event_block_numbers);
    assert!(*reorg_detected.lock().await);

    Ok(())
}

#[tokio::test]
async fn block_confirmations_mitigate_reorgs() -> anyhow::Result<()> {
    let block_confirmations = 5;
    let TestSetup { provider, contract, client, mut stream, anvil: _anvil } =
        setup_scanner(Option::Some(1.0), Option::None, Option::Some(block_confirmations)).await?;

    tokio::spawn(async move { client.start_scanner(BlockNumberOrTag::Latest, None).await });

    provider.anvil_mine(Some(10), None).await?;

    let mut all_tx_hashes = vec![];

    let initial_events = 4_u64;
    for _ in 0..initial_events {
        let receipt = contract.increase().send().await.unwrap().get_receipt().await.unwrap();
        all_tx_hashes.push(receipt.transaction_hash);
    }

    let mut tx_block_pairs = vec![];
    let num_new_events = 2_u64;
    let latest_block = provider.get_block_by_number(BlockNumberOrTag::Latest).await?.unwrap();
    let reorg_depth = 2_u64;
    for _ in 0..num_new_events {
        let tx = contract.increase().into_transaction_request();
        tx_block_pairs.push((TransactionData::JSON(tx), 0));
    }
    let reorg_options = ReorgOptions { depth: reorg_depth, tx_block_pairs };

    provider.anvil_reorg(reorg_options).await.unwrap();
    let new_latest_block =
        provider.get_block_by_number(BlockNumberOrTag::Latest).full().await?.unwrap();

    // sanity checks, block numb stays the same but hash changes
    assert_eq!(new_latest_block.header.number, latest_block.header.number);
    assert_ne!(new_latest_block.header.hash, latest_block.header.hash);

    let new_block = provider
        .get_block_by_number(BlockNumberOrTag::Number(latest_block.header.number - reorg_depth + 1))
        .await?
        .unwrap();
    assert_eq!(new_block.transactions.len() as u64, num_new_events);

    for tx_hash in new_block.transactions.hashes() {
        all_tx_hashes.push(tx_hash);
    }

    // continue mining to simulate incoming blocks
    provider.anvil_mine(Some(10), None).await?;

    let event_tx_hash = Arc::new(Mutex::new(Vec::new()));
    let event_tx_hash_clone = Arc::clone(&event_tx_hash);

    let reorg_detected = Arc::new(Mutex::new(false));
    let reorg_detected_clone = reorg_detected.clone();

    let event_counting = async move {
        while let Some(message) = stream.next().await {
            match message {
                EventScannerMessage::Data(logs) => {
                    let mut guard = event_tx_hash_clone.lock().await;
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

    let final_hashes: Vec<_> = event_tx_hash.lock().await.clone();
    let number_discarded = min(initial_events, reorg_depth);
    let expected_total = (initial_events - number_discarded) + num_new_events;
    assert_eq!(final_hashes.len() as u64, expected_total);

    let confirmed_initial_hashes =
        &all_tx_hashes[0..(initial_events - number_discarded).try_into().unwrap()];

    let reorg_hashes = &all_tx_hashes[usize::try_from(initial_events).unwrap()..
        (initial_events + num_new_events).try_into().unwrap()];

    let expected_hashes: Vec<_> =
        confirmed_initial_hashes.iter().chain(reorg_hashes.iter()).copied().collect();

    assert_eq!(final_hashes, expected_hashes);

    // reorg shouldnt be detected as we are only sending confirmed block
    // reorg_depth < confirmed block
    assert!(!*reorg_detected.lock().await);

    Ok(())
}
