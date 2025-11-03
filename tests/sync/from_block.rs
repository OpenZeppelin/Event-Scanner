use std::{sync::Arc, time::Duration};

use alloy::{eips::BlockNumberOrTag, primitives::U256, providers::ext::AnvilApi};
use event_scanner::{Message, ScannerStatus, assert_next};
use tokio::{sync::Mutex, time::timeout};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};

use crate::common::{
    TestCounter, TestCounterExt, reorg_with_new_count_incr_txs, setup_sync_scanner,
};

#[tokio::test]
async fn replays_historical_then_switches_to_live() -> anyhow::Result<()> {
    let setup = setup_sync_scanner(Some(0.1), None, BlockNumberOrTag::Earliest, 0).await?;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    scanner.start().await?;

    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    // historical events
    assert_next!(
        stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(1) },
            TestCounter::CountIncreased { newCount: U256::from(2) },
            TestCounter::CountIncreased { newCount: U256::from(3) },
        ]
    );

    // chain tip reached
    assert_next!(stream, ScannerStatus::SwitchingToLive);

    // live events
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(4) },]);
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(5) },]);

    Ok(())
}

#[tokio::test]
async fn sync_from_future_block_waits_until_minted() -> anyhow::Result<()> {
    let future_start_block = 4;
    let setup = setup_sync_scanner(None, None, future_start_block, 0).await?;
    let contract = setup.contract;
    let scanner = setup.scanner;

    // Start the scanner in sync mode from the future block
    scanner.start().await?;

    // Send 2 transactions that should not appear in the stream
    _ = contract.increase_and_get_meta().await?;
    _ = contract.increase_and_get_meta().await?;

    // Assert: no messages should be received before reaching the start height
    let inner = setup.stream.into_inner();
    assert!(inner.is_empty());
    let mut stream = ReceiverStream::new(inner);

    // Act: emit an event that will be mined in block == future_start
    let expected = &[contract.increase_and_get_meta().await?];

    // Assert: the first streamed message arrives and contains the expected event
    assert_next!(stream, expected);

    Ok(())
}

#[tokio::test]
async fn block_confirmations_mitigate_reorgs_historic_to_live() -> anyhow::Result<()> {
    // any reorg ≤ 5 should be invisible to consumers
    let block_confirmations = 5;

    let setup =
        setup_sync_scanner(Some(1.0), None, BlockNumberOrTag::Earliest, block_confirmations)
            .await?;
    let provider = setup.provider.clone();
    let contract = setup.contract.clone();

    provider.inner().anvil_mine(Some(10), None).await?;

    let scanner = setup.scanner;
    let mut stream = setup.stream;

    scanner.start().await?;

    // Perform a shallow reorg on the live tail
    let num_initial_events = 4u64;
    let num_new_events = 2u64;
    let reorg_depth = 2u64;
    let same_block = false;

    let all_tx_hashes = reorg_with_new_count_incr_txs(
        &setup.anvil,
        contract.clone(),
        num_initial_events,
        num_new_events,
        reorg_depth,
        same_block,
    )
    .await?;

    provider.inner().anvil_mine(Some(10), None).await?;

    let observed_tx_hashes = Arc::new(Mutex::new(Vec::new()));
    let observed_tx_hashes_clone = Arc::clone(&observed_tx_hashes);

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
                Message::Error(e) => panic!("panic with error {e}"),
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
