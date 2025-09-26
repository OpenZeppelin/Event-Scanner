use alloy::{
    providers::Provider,
    rpc::types::anvil::{ReorgOptions, TransactionData},
};
use std::{sync::Arc, time::Duration};

use tokio::sync::Mutex;

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
    let anvil = spawn_anvil(0.1).unwrap();
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

    let mut expected_event_block_numbers = vec![];

    let initial_events = 5;
    for _ in 0..initial_events {
        let receipt = contract.increase().send().await.unwrap().get_receipt().await.unwrap();
        expected_event_block_numbers.push(receipt.block_number.unwrap());
    }

    let mut tx_block_pairs = vec![];
    let num_new_events = 3;
    let latest_block = provider.get_block_by_number(BlockNumberOrTag::Latest).await?.unwrap();
    let reorg_depth = 5;
    for _ in 0..num_new_events {
        let tx = contract.increase().into_transaction_request();
        // 0 is offset to reorg depth
        tx_block_pairs.push((TransactionData::JSON(tx), 0));
        expected_event_block_numbers.push(latest_block.header.number - reorg_depth + 1);
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
    assert_eq!(new_block.transactions.len(), num_new_events);

    let event_blocks_clone = Arc::clone(&blocks);

    let post_reorg_processing = async move {
        loop {
            let blocks = event_blocks_clone.lock().await;
            if blocks.len() >= initial_events + num_new_events {
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
            initial_events + num_new_events,
            current_len
        );
    }

    let final_blocks: Vec<_> = blocks.lock().await.clone();
    assert!(final_blocks.len() == initial_events + num_new_events);
    assert_eq!(final_blocks, expected_event_block_numbers);
    // sanity check that the block number after the reorg is smaller than the previous block
    assert!(final_blocks[initial_events] < final_blocks[initial_events - 1]);

    Ok(())
}
