use alloy::{
    providers::Provider,
    rpc::types::anvil::{ReorgOptions, TransactionData},
};
use std::{sync::Arc, time::Duration};
use tokio_stream::StreamExt;

use tokio::{sync::Mutex, time::timeout};

use crate::common::{TestCounter, build_provider, deploy_counter, spawn_anvil};
use alloy::{
    eips::BlockNumberOrTag, network::Ethereum, providers::ext::AnvilApi, sol_types::SolEvent,
};
use event_scanner::{event_filter::EventFilter, event_scanner::EventScanner};

#[tokio::test]
async fn reorg_rescans_events_within_same_block() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1).unwrap();
    let provider = build_provider(&anvil).await.unwrap();

    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();

    let filter = EventFilter {
        contract_address: Some(contract_address),
        event: Some(TestCounter::CountIncreased::SIGNATURE.to_owned()),
    };

    let mut client = EventScanner::new().connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;

    let mut stream = client.create_event_stream(filter);

    tokio::spawn(async move { client.start_scanner(BlockNumberOrTag::Latest, None).await });

    let mut expected_event_block_numbers = vec![];

    let initial_events = 5_u64;
    for _ in 0..initial_events {
        let receipt = contract.increase().send().await.unwrap().get_receipt().await.unwrap();
        expected_event_block_numbers.push(receipt.block_number.unwrap());
    }

    let mut tx_block_pairs = vec![];
    let num_new_events = 3_u64;
    let latest_block = provider.get_block_by_number(BlockNumberOrTag::Latest).await?.unwrap();
    let reorg_depth = 5;
    for _ in 0..num_new_events {
        let tx = contract.increase().into_transaction_request();
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
    assert_eq!(new_block.transactions.len() as u64, num_new_events);

    let event_block_count = Arc::new(Mutex::new(Vec::new()));
    let event_block_count_clone = Arc::clone(&event_block_count);
    let event_counting = async move {
        while let Some(Ok(logs)) = stream.next().await {
            let mut guard = event_block_count_clone.lock().await;
            for log in logs {
                if let Some(n) = log.block_number {
                    guard.push(n);
                }
            }
        }
    };

    _ = timeout(Duration::from_secs(2), event_counting).await;

    let final_blocks: Vec<_> = event_block_count.lock().await.clone();
    assert_eq!(final_blocks.len() as u64, initial_events + num_new_events);
    assert_eq!(final_blocks, expected_event_block_numbers);

    Ok(())
}

#[tokio::test]
async fn reorg_rescans_events_with_ascending_blocks() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1).unwrap();
    let provider = build_provider(&anvil).await.unwrap();

    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();

    let filter = EventFilter {
        contract_address: Some(contract_address),
        event: Some(TestCounter::CountIncreased::SIGNATURE.to_owned()),
    };

    let mut client = EventScanner::new().connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;

    let mut stream = client.create_event_stream(filter);

    tokio::spawn(async move { client.start_scanner(BlockNumberOrTag::Latest, None).await });

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
    for i in 0..num_new_events {
        let tx = contract.increase().into_transaction_request();
        tx_block_pairs.push((TransactionData::JSON(tx), i));
        expected_event_block_numbers.push(latest_block.header.number - reorg_depth + 1 + i);
    }
    let reorg_options = ReorgOptions { depth: reorg_depth, tx_block_pairs };

    provider.anvil_reorg(reorg_options).await.unwrap();
    let new_latest_block =
        provider.get_block_by_number(BlockNumberOrTag::Latest).full().await?.unwrap();
    // sanity checks, block numb stays the same but hash changes
    assert_eq!(new_latest_block.header.number, latest_block.header.number);
    assert_ne!(new_latest_block.header.hash, latest_block.header.hash);

    let event_block_count = Arc::new(Mutex::new(Vec::new()));
    let event_block_count_clone = Arc::clone(&event_block_count);
    let event_counting = async move {
        while let Some(Ok(logs)) = stream.next().await {
            let mut guard = event_block_count_clone.lock().await;
            for log in logs {
                if let Some(n) = log.block_number {
                    guard.push(n);
                }
            }
        }
    };

    _ = timeout(Duration::from_secs(2), event_counting).await;

    let final_blocks: Vec<_> = event_block_count.lock().await.clone();
    assert_eq!(final_blocks.len() as u64, initial_events + num_new_events);
    assert_eq!(final_blocks, expected_event_block_numbers);

    Ok(())
}

#[tokio::test]
async fn reorg_depth_one() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1.0).unwrap();
    let provider = build_provider(&anvil).await.unwrap();

    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();

    let filter = EventFilter {
        contract_address: Some(contract_address),
        event: Some(TestCounter::CountIncreased::SIGNATURE.to_owned()),
    };

    let mut client = EventScanner::new().connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;

    let mut stream = client.create_event_stream(filter);

    tokio::spawn(async move { client.start_scanner(BlockNumberOrTag::Latest, None).await });

    let mut expected_event_block_numbers = vec![];

    let initial_events = 4;
    for _ in 0..initial_events {
        let receipt = contract.increase().send().await.unwrap().get_receipt().await.unwrap();
        expected_event_block_numbers.push(receipt.block_number.unwrap());
    }

    let mut tx_block_pairs = vec![];
    let num_new_events = 1;
    let latest_block = provider.get_block_by_number(BlockNumberOrTag::Latest).await?.unwrap();
    let reorg_depth = 1;
    for _ in 0..num_new_events {
        let tx = contract.increase().into_transaction_request();
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

    let event_block_count = Arc::new(Mutex::new(Vec::new()));
    let event_block_count_clone = Arc::clone(&event_block_count);
    let event_counting = async move {
        while let Some(Ok(logs)) = stream.next().await {
            let mut guard = event_block_count_clone.lock().await;
            for log in logs {
                if let Some(n) = log.block_number {
                    guard.push(n);
                }
            }
        }
    };

    _ = timeout(Duration::from_secs(2), event_counting).await;

    let final_blocks: Vec<_> = event_block_count.lock().await.clone();
    assert!(final_blocks.len() == initial_events + num_new_events);
    assert_eq!(final_blocks, expected_event_block_numbers);

    Ok(())
}

#[tokio::test]
async fn reorg_depth_two() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1.0).unwrap();
    let provider = build_provider(&anvil).await.unwrap();

    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();

    let filter = EventFilter {
        contract_address: Some(contract_address),
        event: Some(TestCounter::CountIncreased::SIGNATURE.to_owned()),
    };

    let mut client = EventScanner::new().connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;

    let mut stream = client.create_event_stream(filter);

    tokio::spawn(async move { client.start_scanner(BlockNumberOrTag::Latest, None).await });

    let mut expected_event_block_numbers = vec![];

    let initial_events = 4;
    for _ in 0..initial_events {
        let receipt = contract.increase().send().await.unwrap().get_receipt().await.unwrap();
        expected_event_block_numbers.push(receipt.block_number.unwrap());
    }

    let mut tx_block_pairs = vec![];
    let num_new_events = 1;
    let latest_block = provider.get_block_by_number(BlockNumberOrTag::Latest).await?.unwrap();
    let reorg_depth = 2;
    for _ in 0..num_new_events {
        let tx = contract.increase().into_transaction_request();
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

    let event_block_count = Arc::new(Mutex::new(Vec::new()));
    let event_block_count_clone = Arc::clone(&event_block_count);
    let event_counting = async move {
        while let Some(Ok(logs)) = stream.next().await {
            let mut guard = event_block_count_clone.lock().await;
            for log in logs {
                if let Some(n) = log.block_number {
                    guard.push(n);
                }
            }
        }
    };

    _ = timeout(Duration::from_secs(2), event_counting).await;

    let final_blocks: Vec<_> = event_block_count.lock().await.clone();
    assert!(final_blocks.len() == initial_events + num_new_events);
    assert_eq!(final_blocks, expected_event_block_numbers);

    Ok(())
}
