use std::sync::Arc;

use alloy::{
    eips::BlockNumberOrTag,
    network::Ethereum,
    primitives::{Address, FixedBytes, U256, uint},
    providers::{Provider, RootProvider, ext::AnvilApi},
    rpc::types::Log,
    sol_types::SolEvent,
};
use tokio_stream::StreamExt;

use crate::common::{TestCounter, build_provider, deploy_counter, setup_scanner, spawn_anvil};
use event_scanner::{event_filter::EventFilter, event_scanner::EventScannerMessage};

const ONE: U256 = uint!(1_U256);

async fn collect_events(
    stream: &mut tokio_stream::wrappers::ReceiverStream<EventScannerMessage>,
) -> Vec<Log> {
    loop {
        match stream.next().await {
            Some(EventScannerMessage::Data(logs)) => return logs,
            Some(EventScannerMessage::Error(_)) => continue,
            Some(EventScannerMessage::Status(_)) => continue,
            None => return vec![],
        }
    }
}

fn assert_ordering(
    logs: Vec<Log>,
    expected_first_count: u64,
    expected_hashes: Vec<FixedBytes<32>>,
    expected_address: &Address,
) {
    let mut expected_count = U256::from(expected_first_count);
    for (log, &expected_hash) in logs.iter().zip(expected_hashes.iter()) {
        let event = log.log_decode::<TestCounter::CountIncreased>().expect(
            format!("expected sig: 'TestCounter::CountIncreased', got: {:?}", log.topic0())
                .as_str(),
        );
        assert_eq!(&event.address(), expected_address);
        assert_eq!(event.transaction_hash.unwrap(), expected_hash);
        assert_eq!(expected_count, event.inner.newCount);
        expected_count += ONE;
    }
}

#[tokio::test]
async fn scan_latest_exact_count_returns_last_events_in_order() -> anyhow::Result<()> {
    let setup = setup_scanner(None, None, None).await?;
    let contract = setup.contract;
    let client = setup.client;
    let mut stream = setup.stream;

    // Produce 8 events
    let mut tx_hashes: Vec<FixedBytes<32>> = Vec::new();
    for _ in 0..8u8 {
        let receipt = contract.increase().send().await?.get_receipt().await?;
        tx_hashes.push(receipt.transaction_hash);
    }

    // Ask for the latest 5
    client.scan_latest(5, BlockNumberOrTag::Earliest, BlockNumberOrTag::Latest).await?;

    let logs = collect_events(&mut stream).await;

    assert_eq!(logs.len(), 5, "should receive exactly 5 latest events");

    // Verify exact events (address, signature, tx hashes)
    let expected_first_count = 4;
    let expected_hashes = tx_hashes[3..8].to_vec();

    assert_ordering(logs, expected_first_count, expected_hashes, contract.address());

    Ok(())
}

#[tokio::test]
async fn scan_latest_fewer_available_than_count_returns_all() -> anyhow::Result<()> {
    let setup = setup_scanner(None, None, None).await?;
    let contract = setup.contract;
    let client = setup.client;
    let mut stream = setup.stream;

    // Produce only 3 events
    let mut tx_hashes: Vec<FixedBytes<32>> = Vec::new();
    for _ in 0..3u8 {
        let receipt = contract.increase().send().await?.get_receipt().await?;
        tx_hashes.push(receipt.transaction_hash);
    }

    client.scan_latest(5, BlockNumberOrTag::Earliest, BlockNumberOrTag::Latest).await?;

    let logs = collect_events(&mut stream).await;

    assert_eq!(logs.len(), 3, "should receive only available events");

    // Verify exact events
    let expected_first_count = 1;

    assert_ordering(logs, expected_first_count, tx_hashes, contract.address());

    Ok(())
}

#[tokio::test]
async fn scan_latest_no_events_returns_empty() -> anyhow::Result<()> {
    let setup = setup_scanner(None, None, None).await?;
    let client = setup.client;
    let mut stream = setup.stream;

    client.scan_latest(5, BlockNumberOrTag::Earliest, BlockNumberOrTag::Latest).await?;

    let logs = collect_events(&mut stream).await;

    assert!(logs.is_empty(), "no events should be returned for empty chain range");
    Ok(())
}

#[tokio::test]
async fn scan_latest_respects_range_subset() -> anyhow::Result<()> {
    // Manual setup to control range precisely
    let anvil = spawn_anvil(None)?; // per-tx mining
    let provider: RootProvider = build_provider(&anvil).await?;
    let contract = deploy_counter(Arc::new(provider.clone())).await?;

    // Build custom filter (only our contract, any event)
    let filter = EventFilter::new().with_contract_address(*contract.address());

    let mut client = event_scanner::event_scanner::EventScanner::new()
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;
    let mut stream = client.create_event_stream(filter);

    // Mine 6 events, one per tx (auto-mined), then manually mint 2 empty blocks to widen range
    let mut tx_hashes: Vec<FixedBytes<32>> = Vec::new();
    for _ in 0..6u8 {
        let receipt = contract.increase().send().await?.get_receipt().await?;
        tx_hashes.push(receipt.transaction_hash);
    }
    // manual empty block minting
    provider.anvil_mine(Some(2), None).await?;

    let head = provider.get_block_number().await?;
    // Choose a subrange covering last 4 blocks
    let start = BlockNumberOrTag::from(head - 3);
    let end = BlockNumberOrTag::from(head);

    client.scan_latest(10, start, end).await?;

    let logs = collect_events(&mut stream).await;

    // Expect last 4 emitted events exactly (the 2 empty blocks contain no events)
    assert_eq!(logs.len(), 2);

    let expected_hashes = tx_hashes[4..6].to_vec(); // counts 5..6
    let expected_first_count = 5;

    assert_ordering(logs, expected_first_count, expected_hashes, contract.address());

    Ok(())
}

#[tokio::test]
async fn scan_latest_multiple_listeners_to_same_event_receive_same_results() -> anyhow::Result<()> {
    let setup = setup_scanner(None, None, None).await?;
    let contract = setup.contract;
    let mut client = setup.client;
    let mut stream1 = setup.stream;

    // Add a second listener with the same filter
    let filter2 = EventFilter::new()
        .with_contract_address(*contract.address())
        .with_event(TestCounter::CountIncreased::SIGNATURE);
    let mut stream2 = client.create_event_stream(filter2);

    // Produce 7 events
    let mut tx_hashes: Vec<FixedBytes<32>> = Vec::new();
    for _ in 0..7u8 {
        let receipt = contract.increase().send().await?.get_receipt().await?;
        tx_hashes.push(receipt.transaction_hash);
    }

    client.scan_latest(5, BlockNumberOrTag::Earliest, BlockNumberOrTag::Latest).await?;

    let logs1 = collect_events(&mut stream1).await;
    let logs2 = collect_events(&mut stream2).await;

    assert_eq!(logs1, logs2);

    // since logs are equal, asserting for one, asserts for both
    assert_eq!(5, logs1.len());

    let expected_hashes = tx_hashes[2..7].to_vec();
    let expected_first_count = 3;
    assert_ordering(logs1, expected_first_count, expected_hashes, contract.address());

    Ok(())
}
