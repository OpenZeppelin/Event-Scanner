use alloy::{
    eips::BlockId,
    providers::{Provider, ProviderBuilder, RootProvider, WsConnect},
    rpc::types::{
        Header,
        anvil::{ReorgOptions, TransactionData},
    },
};
use alloy_node_bindings::Anvil;
use std::{collections::HashSet, sync::Arc, time::Duration};

use tokio::sync::Mutex;

use crate::{
    common::{TestCounter, deploy_counter},
    mock_callbacks::BlockOrderingCallback,
};
use alloy::{
    eips::BlockNumberOrTag, network::Ethereum, providers::ext::AnvilApi, sol_types::SolEvent,
};
use event_scanner::{event_filter::EventFilter, event_scanner::EventScannerBuilder};

pub async fn mine_block(provider: &RootProvider<Ethereum>) -> Header {
    let _ = provider.evm_mine(None).await;
    let block = provider.get_block(BlockId::latest()).await.unwrap().unwrap();

    block.header
}

#[tokio::test]
async fn reorg_rescans_events_with_rewind_depth() -> anyhow::Result<()> {
    let anvil = Anvil::new().spawn();
    let provider =
        ProviderBuilder::new().connect_ws(WsConnect::new(anvil.ws_endpoint())).await.unwrap();
    let provider = provider.root().clone();

    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();
    let event_blocks = Arc::new(Mutex::new(Vec::new()));
    let callback = Arc::new(BlockOrderingCallback { blocks: Arc::clone(&event_blocks) });
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
    // Emit initial events
    let initial_events = 5;
    for _ in 0..initial_events {
        let receipt = contract.increase().send().await.unwrap().get_receipt().await.unwrap();
        println!("receipt: {:?}", receipt.block_number);
    }
    let block = provider.get_block(BlockId::latest()).await?.unwrap();
    println!("head: {:?}", block.header.number);
    println!("head hash: {:?}", block.header.hash);
    // Wait for initial events to be processed
    let event_blocks_clone = Arc::clone(&event_blocks);
    let initial_processing = async move {
        loop {
            let blocks = event_blocks_clone.lock().await;
            if blocks.len() >= initial_events {
                break;
            }
            drop(blocks);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };

    assert!(
        tokio::time::timeout(Duration::from_secs(5), initial_processing).await.is_ok(),
        "Initial events not processed in time"
    );
    //
    let _initial_blocks: HashSet<_> = event_blocks.lock().await.clone().into_iter().collect();

    let block_3 = provider.get_block_by_number((3).into()).full().await?.unwrap();
    println!("Block 3 transactions:");
    if let alloy::rpc::types::BlockTransactions::Full(txs) = &block_3.transactions {
        for tx in txs {
            println!("  {:?}", tx.inner.hash());
        }
    }
    //
    // Perform reorg using anvil_reorg with depth 4 and increase transactions in blocks 0,1,2,3
    let mut tx_block_pairs = vec![];
    for i in 0..4 {
        let tx = contract.increase().into_transaction_request();
        tx_block_pairs.push((TransactionData::JSON(tx), 0));
    }
    let reorg_options = ReorgOptions { depth: 4, tx_block_pairs };
    provider.anvil_reorg(reorg_options).await.unwrap();

    // Print tx hashes in block 3 (last reorged block)
    let block_3 = provider.get_block_by_number((3).into()).full().await?.unwrap();
    println!("Block 3 transactions:");
    if let alloy::rpc::types::BlockTransactions::Full(txs) = &block_3.transactions {
        for tx in txs {
            println!("  {:?}", tx.inner.hash());
        }
    }

    // let block = provider.get_block_by_number(BlockNumberOrTag::Latest).await?.unwrap();
    // println!("head: {:?}", block.header.number);
    // println!("head hash: {:?}", block.header.hash);
    //
    // // Emit new events in the reorged blocks
    // let post_reorg_events = 2;
    // for _ in 0..post_reorg_events {
    //     let receipt = contract.increase().send().await.unwrap().get_receipt().await.unwrap();
    //     println!("receipt: {:?}", receipt.block_number);
    // }
    //
    // // Wait for post-reorg events to be rescanned
    // let event_blocks_clone = Arc::clone(&event_blocks);
    // let post_reorg_processing = async move {
    //     loop {
    //         let blocks = event_blocks_clone.lock().await;
    //         // We expect more events due to rescan
    //         if blocks.len() >= initial_events + post_reorg_events {
    //             break;
    //         }
    //         drop(blocks);
    //         tokio::time::sleep(Duration::from_millis(100)).await;
    //     }
    // };
    //
    // if tokio::time::timeout(Duration::from_secs(5), post_reorg_processing).await.is_err() {
    //     let current_len = event_blocks.lock().await.len();
    //     panic!(
    //         "Post-reorg events not rescanned in time. Expected at least {}, got {}",
    //         initial_events + post_reorg_events,
    //         current_len
    //     );
    // }
    // //
    // // Verify that events were rescanned
    // let final_blocks: Vec<_> = event_blocks.lock().await.clone();
    // assert!(final_blocks.len() >= initial_events + post_reorg_events);

    Ok(())
}
