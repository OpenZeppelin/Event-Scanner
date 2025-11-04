use anyhow::Ok;
use std::{sync::Arc, time::Duration};
use tokio_stream::StreamExt;

use tokio::{sync::Mutex, time::timeout};

use crate::common::{
    LiveScannerSetup, TestCounter::CountIncreased, reorg_with_new_count_incr_txs,
    setup_live_scanner,
};
use alloy::{
    primitives::U256,
    providers::ext::AnvilApi,
    rpc::types::anvil::{ReorgOptions, TransactionData},
};
use event_scanner::{Message, ScannerStatus, assert_empty, assert_next};

#[tokio::test]
async fn reorg_rescans_events_within_same_block() -> anyhow::Result<()> {
    let LiveScannerSetup { provider, contract, scanner, mut stream, anvil: _anvil } =
        setup_live_scanner(None, None, 0).await?;

    scanner.start().await?;

    // emit initial events
    for _ in 0..5 {
        contract.increase().send().await?.watch().await?;
    }

    // assert initial events are emitted as expected
    assert_next!(stream, &[CountIncreased { newCount: U256::from(1) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(2) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(3) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(4) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(5) }]);
    let mut stream = assert_empty!(stream);

    // reorg the chain
    let tx_block_pairs = vec![
        (TransactionData::JSON(contract.increase().into_transaction_request()), 0),
        (TransactionData::JSON(contract.increase().into_transaction_request()), 0),
        (TransactionData::JSON(contract.increase().into_transaction_request()), 0),
    ];
    provider.anvil_reorg(ReorgOptions { depth: 4, tx_block_pairs }).await?;

    // assert expected messages post-reorg
    assert_next!(stream, ScannerStatus::ReorgDetected);
    assert_next!(
        stream,
        &[
            CountIncreased { newCount: U256::from(2) },
            CountIncreased { newCount: U256::from(3) },
            CountIncreased { newCount: U256::from(4) },
        ]
    );
    assert_empty!(stream);

    Ok(())
}

#[tokio::test]
async fn reorg_rescans_events_with_ascending_blocks() -> anyhow::Result<()> {
    let LiveScannerSetup { provider, contract, scanner, mut stream, anvil: _anvil } =
        setup_live_scanner(None, None, 0).await?;

    scanner.start().await?;

    // emit initial events
    for _ in 0..5 {
        contract.increase().send().await?.watch().await?;
    }

    // assert initial events are emitted as expected
    assert_next!(stream, &[CountIncreased { newCount: U256::from(1) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(2) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(3) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(4) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(5) }]);
    let mut stream = assert_empty!(stream);

    // reorg the chain
    let tx_block_pairs = vec![
        (TransactionData::JSON(contract.increase().into_transaction_request()), 0),
        (TransactionData::JSON(contract.increase().into_transaction_request()), 1),
        (TransactionData::JSON(contract.increase().into_transaction_request()), 2),
    ];
    provider.anvil_reorg(ReorgOptions { depth: 4, tx_block_pairs }).await?;

    // assert expected messages post-reorg
    assert_next!(stream, ScannerStatus::ReorgDetected);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(2) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(3) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(4) }]);
    assert_empty!(stream);

    Ok(())
}

#[tokio::test]
async fn reorg_depth_one() -> anyhow::Result<()> {
    let LiveScannerSetup { provider, contract, scanner, mut stream, anvil: _anvil } =
        setup_live_scanner(Option::Some(0.1), Option::None, 0).await?;

    scanner.start().await?;

    let num_initial_events = 4;

    let reorg_depth = 1;
    let num_new_events = 1;
    let same_block = true;

    let expected_event_tx_hashes = reorg_with_new_count_incr_txs(
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
                Message::Data(logs) => {
                    let mut guard = event_block_count_clone.lock().await;
                    for log in logs {
                        if let Some(n) = log.transaction_hash {
                            guard.push(n);
                        }
                    }
                }
                Message::Error(e) => {
                    panic!("panic with error {e}");
                }
                Message::Status(info) => {
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
    let LiveScannerSetup { provider, contract, scanner, mut stream, anvil: _anvil } =
        setup_live_scanner(Option::Some(0.1), Option::None, 0).await?;

    scanner.start().await?;

    let num_initial_events = 4;

    let num_new_events = 1;
    let reorg_depth = 2;

    let same_block = true;
    let expected_event_tx_hashes = reorg_with_new_count_incr_txs(
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
                Message::Data(logs) => {
                    let mut guard = event_block_count_clone.lock().await;
                    for log in logs {
                        if let Some(n) = log.transaction_hash {
                            guard.push(n);
                        }
                    }
                }
                Message::Error(e) => {
                    panic!("panic with error {e}");
                }
                Message::Status(info) => {
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
    let LiveScannerSetup { provider, contract, scanner, mut stream, anvil: _anvil } =
        setup_live_scanner(Option::Some(1.0), Option::None, block_confirmations).await?;

    scanner.start().await?;

    provider.anvil_mine(Some(10), None).await?;

    let num_initial_events = 4_u64;
    let num_new_events = 2_u64;
    // reorg depth is less than confirmations -> mitigated
    let reorg_depth = 2_u64;
    let same_block = true;

    let all_tx_hashes = reorg_with_new_count_incr_txs(
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
                Message::Data(logs) => {
                    let mut guard = observed_tx_hashes_clone.lock().await;
                    for log in logs {
                        if let Some(n) = log.transaction_hash {
                            guard.push(n);
                        }
                    }
                }
                Message::Error(e) => {
                    panic!("panic with error {e}");
                }
                Message::Status(info) => {
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
