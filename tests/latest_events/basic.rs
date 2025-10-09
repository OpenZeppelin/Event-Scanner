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

#[tokio::test]
async fn scan_latest_different_filters_receive_different_results() -> anyhow::Result<()> {
    let setup = setup_scanner(None, None, None).await?;
    let contract = setup.contract;
    let mut client = setup.client;

    // First listener for CountDecreased
    let filter_inc = EventFilter::new()
        .with_contract_address(*contract.address())
        .with_event(TestCounter::CountIncreased::SIGNATURE);
    let mut stream_inc = client.create_event_stream(filter_inc);

    // Second listener for CountDecreased
    let filter_dec = EventFilter::new()
        .with_contract_address(*contract.address())
        .with_event(TestCounter::CountDecreased::SIGNATURE);
    let mut stream_dec = client.create_event_stream(filter_dec);

    // Produce 5 increases, then 2 decreases
    let mut inc_hashes: Vec<FixedBytes<32>> = Vec::new();
    for _ in 0..5u8 {
        let r = contract.increase().send().await?.get_receipt().await?;
        inc_hashes.push(r.transaction_hash);
    }
    let mut dec_hashes: Vec<FixedBytes<32>> = Vec::new();
    for _ in 0..2u8 {
        let r = contract.decrease().send().await?.get_receipt().await?;
        dec_hashes.push(r.transaction_hash);
    }

    // Ask for latest 3 across the full range: each filtered listener should receive their own last
    // 3 events
    client.scan_latest(3, BlockNumberOrTag::Earliest, BlockNumberOrTag::Latest).await?;

    let logs_inc = collect_events(&mut stream_inc).await;
    let logs_dec = collect_events(&mut stream_dec).await;

    // Should be different sequences and lengths match the requested count (or fewer if not enough)
    assert_eq!(logs_inc.len(), 3);
    assert_eq!(logs_dec.len(), 2); // only 2 decreases exist

    // Validate increases: expect counts 3,4,5 and the corresponding tx hashes from inc_hashes[2..5]
    let expected_hashes_inc = inc_hashes[2..5].to_vec();
    assert_ordering(logs_inc, 3, expected_hashes_inc, contract.address());

    // Validate decreases: expect counts 4,3 (after two decreases)
    let mut expected_count_dec = U256::from(4);
    for (log, &expected_hash) in logs_dec.iter().zip(dec_hashes.iter()) {
        let ev = log.log_decode::<TestCounter::CountDecreased>()?;
        assert_eq!(&ev.address(), contract.address());
        assert_eq!(ev.transaction_hash.unwrap(), expected_hash);
        assert_eq!(ev.inner.newCount, expected_count_dec);
        expected_count_dec -= ONE;
    }

    Ok(())
}

#[tokio::test]
async fn scan_latest_ignores_reorg_and_returns_canonical_latest() -> anyhow::Result<()> {
    let setup = setup_scanner(None, None, None).await?;
    let contract = setup.contract;
    let provider = setup.provider;
    let client = setup.client;
    let mut stream = setup.stream;

    // Create 4 events, then reorg last 2 blocks with 2 new txs
    let _initial_hashes = crate::common::reorg_with_new_count_incr_txs(
        provider.clone(),
        contract.clone(),
        4,     // num_initial_events
        2,     // num_new_events (these will be the canonical latest)
        2,     // reorg depth
        false, // place new events in separate blocks
    )
    .await?;

    // Now query latest 2 from full range
    client.scan_latest(2, BlockNumberOrTag::Earliest, BlockNumberOrTag::Latest).await?;

    let logs = collect_events(&mut stream).await;
    assert_eq!(logs.len(), 2);

    // After reorg, counts should be 3 and 4 (the chain kept the first two increments; then two new
    // increments) Validate exact address, tx hashes ordering via decode and count values.
    let mut expected_count = U256::from(3u64);
    for log in &logs {
        let ev = log.log_decode::<TestCounter::CountIncreased>()?;
        assert_eq!(&ev.address(), contract.address());
        assert_eq!(ev.inner.newCount, expected_count);
        expected_count += ONE;
    }

    Ok(())
}
