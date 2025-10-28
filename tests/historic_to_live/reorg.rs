use std::{sync::Arc, time::Duration};

use alloy::providers::ext::AnvilApi;
use event_scanner::{Message, types::ScannerStatus};
use tokio::{sync::Mutex, time::timeout};
use tokio_stream::StreamExt;

use crate::common::{reorg_with_new_count_incr_txs, setup_sync_scanner};

#[tokio::test]
async fn block_confirmations_mitigate_reorgs_historic_to_live() -> anyhow::Result<()> {
    // any reorg ≤ 5 should be invisible to consumers
    let block_confirmations = 5;

    let setup = setup_sync_scanner(Some(1.0), None, block_confirmations).await?;
    let provider = setup.provider.clone();
    let contract = setup.contract.clone();

    provider.anvil_mine(Some(10), None).await?;

    let scanner = setup.scanner;
    let mut stream = setup.stream;

    scanner.start().await?;

    // Perform a shallow reorg on the live tail
    let num_initial_events = 4u64;
    let num_new_events = 2u64;
    let reorg_depth = 2u64;
    let same_block = false;

    let all_tx_hashes = reorg_with_new_count_incr_txs(
        provider.clone(),
        contract.clone(),
        num_initial_events,
        num_new_events,
        reorg_depth,
        same_block,
    )
    .await?;

    provider.anvil_mine(Some(10), None).await?;

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
