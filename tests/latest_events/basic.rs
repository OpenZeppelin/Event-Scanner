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
            None => return vec![],
            _ => {}
        }
    }
}

fn assert_ordering(
    logs: &[Log],
    expected_first_count: u64,
    expected_hashes: &[FixedBytes<32>],
    expected_address: &Address,
) {
    let mut expected_count = U256::from(expected_first_count);
    for (log, &expected_hash) in logs.iter().zip(expected_hashes.iter()) {
        let event = log.log_decode::<TestCounter::CountIncreased>().unwrap_or_else(|_| {
            panic!("expected sig: 'TestCounter::CountIncreased', got: {:?}", log.topic0())
        });
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
    client.scan_latest(5).await?;

    let logs = collect_events(&mut stream).await;

    assert_eq!(logs.len(), 5, "should receive exactly 5 latest events");

    // Verify exact events (address, signature, tx hashes)
    let expected_first_count = 4;
    let expected_hashes = &tx_hashes[3..8];

    assert_ordering(&logs, expected_first_count, expected_hashes, contract.address());

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

    client.scan_latest(5).await?;

    let logs = collect_events(&mut stream).await;

    assert_eq!(logs.len(), 3, "should receive only available events");

    // Verify exact events
    let expected_first_count = 1;

    assert_ordering(&logs, expected_first_count, &tx_hashes, contract.address());

    Ok(())
}

#[tokio::test]
async fn scan_latest_no_events_returns_empty() -> anyhow::Result<()> {
    let setup = setup_scanner(None, None, None).await?;
    let client = setup.client;
    let mut stream = setup.stream;

    client.scan_latest(5).await?;

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

    client.scan_latest_in_range(10, start, end).await?;

    let logs = collect_events(&mut stream).await;

    // Expect last 4 emitted events exactly (the 2 empty blocks contain no events)
    assert_eq!(logs.len(), 2);

    let expected_hashes = &tx_hashes[4..6]; // counts 5..6
    let expected_first_count = 5;

    assert_ordering(&logs, expected_first_count, expected_hashes, contract.address());

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

    client.scan_latest(5).await?;

    let logs1 = collect_events(&mut stream1).await;
    let logs2 = collect_events(&mut stream2).await;

    assert_eq!(logs1, logs2);

    // since logs are equal, asserting for one, asserts for both
    assert_eq!(5, logs1.len());

    let expected_hashes = &tx_hashes[2..7];
    let expected_first_count = 3;
    assert_ordering(&logs1, expected_first_count, expected_hashes, contract.address());

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
    client.scan_latest(3).await?;

    let logs_inc = collect_events(&mut stream_inc).await;
    let logs_dec = collect_events(&mut stream_dec).await;

    // Should be different sequences and lengths match the requested count (or fewer if not enough)
    assert_eq!(logs_inc.len(), 3);
    assert_eq!(logs_dec.len(), 2); // only 2 decreases exist

    // Validate increases: expect counts 3,4,5 and the corresponding tx hashes from inc_hashes[2..5]
    let expected_hashes_inc = inc_hashes[2..5].to_vec();
    assert_ordering(&logs_inc, 3, &expected_hashes_inc, contract.address());

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
async fn scan_latest_mixed_events_and_filters_return_correct_streams() -> anyhow::Result<()> {
    let setup = setup_scanner(None, None, None).await?;
    let contract = setup.contract;
    let mut client = setup.client;
    let mut inc_stream = setup.stream; // CountIncreased by default

    // Add a CountDecreased listener
    let filter_dec = EventFilter::new()
        .with_contract_address(*contract.address())
        .with_event(TestCounter::CountDecreased::SIGNATURE);
    let mut dec_stream = client.create_event_stream(filter_dec);

    // Sequence: inc(1), inc(2), dec(1), inc(2), dec(1)
    let mut inc_hashes: Vec<FixedBytes<32>> = Vec::new();
    let mut dec_hashes: Vec<FixedBytes<32>> = Vec::new();

    // inc -> 1
    inc_hashes.push(contract.increase().send().await?.get_receipt().await?.transaction_hash);
    // inc -> 2
    inc_hashes.push(contract.increase().send().await?.get_receipt().await?.transaction_hash);
    // dec -> 1
    dec_hashes.push(contract.decrease().send().await?.get_receipt().await?.transaction_hash);
    // inc -> 2
    inc_hashes.push(contract.increase().send().await?.get_receipt().await?.transaction_hash);
    // dec -> 1
    dec_hashes.push(contract.decrease().send().await?.get_receipt().await?.transaction_hash);

    client.scan_latest(2).await?;

    let inc_logs = collect_events(&mut inc_stream).await;
    let dec_logs = collect_events(&mut dec_stream).await;

    assert_eq!(inc_logs.len(), 2);
    assert_eq!(dec_logs.len(), 2);

    // Validate increases: counts [2,2] with matching hashes
    let expected_inc_counts = [2, 2];
    for ((log, &expected_hash), &expected_count) in
        inc_logs.iter().zip(inc_hashes[1..].iter()).zip(expected_inc_counts.iter())
    {
        let ev = log.log_decode::<TestCounter::CountIncreased>()?;
        assert_eq!(&ev.address(), contract.address());
        assert_eq!(ev.transaction_hash.unwrap(), expected_hash);
        assert_eq!(ev.inner.newCount, U256::from(expected_count));
    }

    // Validate decreases: counts [1,1] with matching hashes
    let expected_dec_counts = [1, 1];
    for ((log, &expected_hash), &expected_count) in
        dec_logs.iter().zip(dec_hashes.iter()).zip(expected_dec_counts.iter())
    {
        let ev = log.log_decode::<TestCounter::CountDecreased>()?;
        assert_eq!(&ev.address(), contract.address());
        assert_eq!(ev.transaction_hash.unwrap(), expected_hash);
        assert_eq!(ev.inner.newCount, U256::from(expected_count));
    }

    Ok(())
}

#[tokio::test]
async fn scan_latest_cross_contract_filtering() -> anyhow::Result<()> {
    // Manual setup to deploy two contracts
    let anvil = spawn_anvil(None)?;
    let provider: RootProvider = build_provider(&anvil).await?;
    let contract_a = deploy_counter(Arc::new(provider.clone())).await?;
    let contract_b = deploy_counter(Arc::new(provider.clone())).await?;

    // Listener only for contract A CountIncreased
    let filter_a = EventFilter::new()
        .with_contract_address(*contract_a.address())
        .with_event(TestCounter::CountIncreased::SIGNATURE);

    let mut client = event_scanner::event_scanner::EventScanner::new()
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;
    let mut stream_a = client.create_event_stream(filter_a);

    // Emit interleaved events from A and B: A(1), B(1), A(2), B(2), A(3)
    let mut a_hashes: Vec<FixedBytes<32>> = Vec::new();
    a_hashes.push(contract_a.increase().send().await?.get_receipt().await?.transaction_hash);
    let _ = contract_b.increase().send().await?.get_receipt().await?; // ignored by filter
    a_hashes.push(contract_a.increase().send().await?.get_receipt().await?.transaction_hash);
    let _ = contract_b.increase().send().await?.get_receipt().await?; // ignored by filter
    a_hashes.push(contract_a.increase().send().await?.get_receipt().await?.transaction_hash);

    client.scan_latest(5).await?;

    let logs_a = collect_events(&mut stream_a).await;
    assert_eq!(logs_a.len(), 3);

    // Validate only contract A logs with counts 1,2,3
    for ((log, &expected_hash), expected_count) in logs_a.iter().zip(a_hashes.iter()).zip(1u64..=3)
    {
        let ev = log.log_decode::<TestCounter::CountIncreased>()?;
        assert_eq!(&ev.address(), contract_a.address());
        assert_eq!(ev.transaction_hash.unwrap(), expected_hash);
        assert_eq!(ev.inner.newCount, U256::from(expected_count));
    }

    Ok(())
}

#[tokio::test]
async fn scan_latest_large_gaps_and_empty_ranges() -> anyhow::Result<()> {
    // Manual setup to mine empty blocks
    let anvil = spawn_anvil(None)?;
    let provider: RootProvider = build_provider(&anvil).await?;
    let contract = deploy_counter(Arc::new(provider.clone())).await?;

    let filter = EventFilter::new()
        .with_contract_address(*contract.address())
        .with_event(TestCounter::CountIncreased::SIGNATURE);

    let mut client = event_scanner::event_scanner::EventScanner::new()
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;
    let mut stream = client.create_event_stream(filter);

    // Emit 2 events
    let mut hashes: Vec<FixedBytes<32>> = Vec::new();
    for _ in 0..2u8 {
        hashes.push(contract.increase().send().await?.get_receipt().await?.transaction_hash);
    }
    // Mine 10 empty blocks
    provider.anvil_mine(Some(10), None).await?;
    // Emit 1 more event
    hashes.push(contract.increase().send().await?.get_receipt().await?.transaction_hash);

    let head = provider.get_block_number().await?;
    let start = BlockNumberOrTag::from(head - 12);
    let end = BlockNumberOrTag::from(head);

    client.scan_latest_in_range(5, start, end).await?;
    let logs = collect_events(&mut stream).await;

    assert_eq!(logs.len(), 3);
    // Expect counts 1,2,3 and hashes in order
    assert_ordering(&logs, 1, &hashes, contract.address());

    Ok(())
}

#[tokio::test]
async fn scan_latest_boundary_range_single_block() -> anyhow::Result<()> {
    let setup = setup_scanner(None, None, None).await?;
    let provider = setup.provider;
    let contract = setup.contract;
    let client = setup.client;
    let mut stream = setup.stream;

    // Each tx auto-mines a block: we will target the middle block specifically
    let mut receipt_blocks: Vec<(FixedBytes<32>, u64)> = Vec::new();
    for _ in 0..3u8 {
        let r = contract.increase().send().await?.get_receipt().await?;
        // fetch the mined block number from provider to be exact
        let tx = provider.get_transaction_by_hash(r.transaction_hash).await?.unwrap();
        let block_num = tx.block_number.unwrap();
        receipt_blocks.push((r.transaction_hash, block_num));
    }

    // Pick the middle tx's block number
    let (_mid_hash, mid_block) = receipt_blocks[1];
    let start = BlockNumberOrTag::from(mid_block);
    let end = BlockNumberOrTag::from(mid_block);

    client.scan_latest_in_range(5, start, end).await?;
    let logs = collect_events(&mut stream).await;

    // Expect exactly the middle event only, with count 2
    assert_eq!(logs.len(), 1);
    let ev = logs[0].log_decode::<TestCounter::CountIncreased>()?;
    assert_eq!(&ev.address(), contract.address());
    assert_eq!(ev.inner.newCount, U256::from(2u64));

    Ok(())
}
