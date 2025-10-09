use std::sync::Arc;

use alloy::{
    eips::BlockNumberOrTag,
    network::Ethereum,
    primitives::FixedBytes,
    providers::{Provider, RootProvider, ext::AnvilApi},
    rpc::types::Log,
    sol_types::SolEvent,
};
use tokio_stream::StreamExt;

use crate::common::{TestCounter, build_provider, deploy_counter, setup_scanner, spawn_anvil};
use event_scanner::{event_filter::EventFilter, event_scanner::EventScannerMessage};

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

fn assert_ordering(logs: &[Log]) {
    for w in logs.windows(2) {
        let a = (&w[0].block_number.unwrap(), &w[0].transaction_index.unwrap());
        let b = (&w[1].block_number.unwrap(), &w[1].transaction_index.unwrap());
        assert!(a <= b, "events must be ordered ascending by (block, tx index)");
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

    // Ensure logs are in ascending block/tx order
    assert_ordering(&logs);

    // Verify exact events (address, signature, tx hashes)
    let expected_hashes = tx_hashes[3..8].to_vec();
    let sig = TestCounter::CountIncreased::SIGNATURE_HASH;
    for (log, expected_hash) in logs.iter().zip(expected_hashes.iter()) {
        assert_eq!(log.address(), *contract.address());
        assert_eq!(log.topics()[0], sig);
        assert_eq!(log.transaction_hash.unwrap(), *expected_hash);
    }

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
    let sig = TestCounter::CountIncreased::SIGNATURE_HASH;
    for (log, expected_hash) in logs.iter().zip(tx_hashes.iter()) {
        assert_eq!(log.address(), *contract.address());
        assert_eq!(log.topics()[0], sig);
        assert_eq!(log.transaction_hash.unwrap(), *expected_hash);
    }
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
    let sig = TestCounter::CountIncreased::SIGNATURE_HASH;
    let expected_hashes = tx_hashes[4..6].to_vec(); // counts 5..6
    for (log, expected_hash) in logs.iter().zip(expected_hashes.iter()) {
        assert_eq!(log.address(), *contract.address());
        assert_eq!(log.topics()[0], sig);
        assert_eq!(log.transaction_hash.unwrap(), *expected_hash);
    }

    Ok(())
}

#[tokio::test]
async fn scan_latest_multiple_listeners_receive_results() -> anyhow::Result<()> {
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
    for _ in 0..7u8 {
        let _ = contract.increase().send().await?.get_receipt().await?;
    }

    client.scan_latest(5, BlockNumberOrTag::Earliest, BlockNumberOrTag::Latest).await?;

    let logs1 = collect_events(&mut stream1).await;
    let logs2 = collect_events(&mut stream2).await;

    assert_eq!(logs1.len(), 5);
    assert_eq!(logs2.len(), 5);
    assert_eq!(
        logs1.iter().map(|l| (l.block_number, l.transaction_index)).collect::<Vec<_>>(),
        logs2.iter().map(|l| (l.block_number, l.transaction_index)).collect::<Vec<_>>(),
        "both listeners should receive identical results"
    );

    Ok(())
}
