use alloy::{
    providers::Provider,
    rpc::types::anvil::{ReorgOptions, TransactionData},
};
use anyhow::bail;
use std::{sync::Arc, time::Duration};

use tokio::{
    sync::Mutex,
    time::{sleep, timeout},
};

use crate::{
    common::{TestCounter, build_provider, deploy_counter, spawn_anvil},
    mock_callbacks::BlockOrderingCallback,
};
use alloy::{
    eips::BlockNumberOrTag, network::Ethereum, providers::ext::AnvilApi, sol_types::SolEvent,
};
use event_scanner::{event_filter::EventFilter, event_scanner::EventScannerBuilder};

#[tokio::test]
async fn reorg_rescans_events_with_rewind_depth() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1.0).unwrap();
    let provider = build_provider(&anvil).await.unwrap();

    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();
    let blocks = Arc::new(Mutex::new(Vec::new()));
    let callback = Arc::new(BlockOrderingCallback { blocks: Arc::clone(&blocks) });
    let filter = EventFilter {
        contract_address: Some(contract_address),
        event: Some(TestCounter::CountIncreased::SIGNATURE.to_owned()),
        callback,
    };

    let mut scanner = EventScannerBuilder::new()
        .with_event_filter(filter)
        .with_reorg_rewind_depth(6)
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;

    tokio::spawn(async move { scanner.start(BlockNumberOrTag::Latest, None).await });

    let initial_events = 5;
    for _ in 0..initial_events {
        contract.increase().send().await.unwrap().watch().await?;
    }

    let blocks_clone = Arc::clone(&blocks);
    let event_counting = async move {
        while blocks_clone.lock().await.len() < initial_events {
            sleep(Duration::from_millis(100)).await;
        }
    };

    if timeout(Duration::from_secs(1), event_counting).await.is_err() {
        bail!("expected {initial_events} events, got {}", blocks.lock().await.len());
    }

    let mut tx_block_pairs = vec![];
    for _ in 0..4 {
        let tx = contract.increase().into_transaction_request();
        tx_block_pairs.push((TransactionData::JSON(tx), 0));
    }
    let reorg_options = ReorgOptions { depth: 4, tx_block_pairs };

    provider.anvil_reorg(reorg_options).await.unwrap();

    // Wait for post-reorg events to be rescanned
    let event_blocks_clone = Arc::clone(&blocks);
    let post_reorg_processing = async move {
        loop {
            let blocks = event_blocks_clone.lock().await;
            // We expect more events due to rescan
            if blocks.len() >= initial_events + 4 {
                break;
            }
            drop(blocks);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };

    if tokio::time::timeout(Duration::from_secs(5), post_reorg_processing).await.is_err() {
        let current_len = blocks.lock().await.len();
        panic!(
            "Post-reorg events not rescanned in time. Expected at least {}, got {}",
            initial_events + 4,
            current_len
        );
    }
    //
    // Verify that events were rescanned
    println!("Final blocks: {:?}", blocks.lock().await);
    let final_blocks: Vec<_> = blocks.lock().await.clone();
    assert!(final_blocks.len() >= initial_events + 4);

    Ok(())
}
