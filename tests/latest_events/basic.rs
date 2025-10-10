use std::sync::Arc;

use alloy::{
    eips::BlockNumberOrTag,
    network::Ethereum,
    primitives::{FixedBytes, U256},
    providers::{Provider, RootProvider, ext::AnvilApi},
    sol_types::SolEvent,
};

use crate::{
    assert_next_logs,
    common::{TestCounter, build_provider, deploy_counter, setup_scanner, spawn_anvil},
    macros::LogMetadata,
};
use event_scanner::event_filter::EventFilter;

macro_rules! increase {
    ($contract: expr) => {{
        let receipt = $contract.increase().send().await?.get_receipt().await?;
        let tx_hash = receipt.transaction_hash;
        let new_count = receipt.decoded_log::<TestCounter::CountIncreased>().unwrap().data.newCount;
        LogMetadata {
            event: TestCounter::CountIncreased { newCount: U256::from(new_count) },
            address: *$contract.address(),
            tx_hash,
        }
    }};
}

macro_rules! decrease {
    ($contract: expr) => {{
        let receipt = $contract.decrease().send().await?.get_receipt().await?;
        let tx_hash = receipt.transaction_hash;
        let new_count = receipt.decoded_log::<TestCounter::CountDecreased>().unwrap().data.newCount;
        LogMetadata {
            event: TestCounter::CountDecreased { newCount: U256::from(new_count) },
            address: *$contract.address(),
            tx_hash,
        }
    }};
}

#[tokio::test]
async fn scan_latest_exact_count_returns_last_events_in_order() -> anyhow::Result<()> {
    let setup = setup_scanner(None, None, None).await?;
    let contract = setup.contract;
    let client = setup.client;
    let mut stream = setup.stream;

    // Produce 8 events
    _ = increase!(contract);
    _ = increase!(contract);
    _ = increase!(contract);

    let expected = &[
        increase!(contract),
        increase!(contract),
        increase!(contract),
        increase!(contract),
        increase!(contract),
    ];

    // Ask for the latest 5
    client.scan_latest(5).await?;

    assert_next_logs!(stream, expected);

    Ok(())
}

#[tokio::test]
async fn scan_latest_fewer_available_than_count_returns_all() -> anyhow::Result<()> {
    let setup = setup_scanner(None, None, None).await?;
    let contract = setup.contract;
    let client = setup.client;
    let mut stream = setup.stream;

    // Produce only 3 events
    let mut expected = vec![];
    expected.push(increase!(contract));
    expected.push(increase!(contract));
    expected.push(increase!(contract));

    client.scan_latest(5).await?;

    let expected = &expected;

    assert_next_logs!(stream, expected);

    Ok(())
}

#[tokio::test]
async fn scan_latest_no_events_returns_empty() -> anyhow::Result<()> {
    let setup = setup_scanner(None, None, None).await?;
    let client = setup.client;
    let mut stream = setup.stream;

    client.scan_latest(5).await?;

    let expected: &[LogMetadata<TestCounter::CountIncreased>] = &[];
    assert_next_logs!(stream, expected);

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
    _ = increase!(contract);
    _ = increase!(contract);
    _ = increase!(contract);
    _ = increase!(contract);

    let expected = &[increase!(contract), increase!(contract)];

    // manual empty block minting
    provider.anvil_mine(Some(2), None).await?;

    let head = provider.get_block_number().await?;
    // Choose a subrange covering last 4 blocks
    let start = BlockNumberOrTag::from(head - 3);
    let end = BlockNumberOrTag::from(head);

    client.scan_latest_in_range(10, start, end).await?;

    assert_next_logs!(stream, expected);

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
    _ = increase!(contract);
    _ = increase!(contract);

    let expected = &[
        increase!(contract),
        increase!(contract),
        increase!(contract),
        increase!(contract),
        increase!(contract),
    ];

    client.scan_latest(5).await?;

    assert_next_logs!(stream1, expected);
    assert_next_logs!(stream2, expected);

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
    _ = increase!(contract);
    _ = increase!(contract);
    let inc_log_meta = vec![increase!(contract), increase!(contract), increase!(contract)];

    let mut dec_log_meta = vec![];
    dec_log_meta.push(decrease!(contract));
    dec_log_meta.push(decrease!(contract));

    // Ask for latest 3 across the full range: each filtered listener should receive their own last
    // 3 events
    client.scan_latest(3).await?;

    let expected = &inc_log_meta;
    assert_next_logs!(stream_inc, expected);

    let expected = &dec_log_meta;
    assert_next_logs!(stream_dec, expected);

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
    let mut inc_log_meta = Vec::new();
    let mut dec_log_meta = Vec::new();

    // inc -> 1
    _ = increase!(contract);

    // inc -> 2
    inc_log_meta.push(increase!(contract));
    // dec -> 1
    dec_log_meta.push(decrease!(contract));
    // inc -> 2
    inc_log_meta.push(increase!(contract));
    // dec -> 1
    dec_log_meta.push(decrease!(contract));

    client.scan_latest(2).await?;

    let expected = &inc_log_meta;
    assert_next_logs!(inc_stream, expected);

    let expected = &dec_log_meta;
    assert_next_logs!(dec_stream, expected);

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
    let mut a_log_meta: Vec<LogMetadata<TestCounter::CountIncreased>> = Vec::new();
    a_log_meta.push(increase!(contract_a));
    let _ = contract_b.increase().send().await?.get_receipt().await?; // ignored by filter
    a_log_meta.push(increase!(contract_a));
    let _ = contract_b.increase().send().await?.get_receipt().await?; // ignored by filter
    a_log_meta.push(increase!(contract_a));

    client.scan_latest(5).await?;

    assert_next_logs!(stream_a, &a_log_meta);

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
    let mut log_meta = vec![];
    log_meta.push(increase!(contract));
    log_meta.push(increase!(contract));

    // Mine 10 empty blocks
    provider.anvil_mine(Some(10), None).await?;
    // Emit 1 more event
    log_meta.push(increase!(contract));

    let head = provider.get_block_number().await?;
    let start = BlockNumberOrTag::from(head - 12);
    let end = BlockNumberOrTag::from(head);

    client.scan_latest_in_range(5, start, end).await?;

    assert_next_logs!(stream, &log_meta);

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

    let expected = &[LogMetadata {
        event: TestCounter::CountIncreased { newCount: U256::from(2) },
        address: *contract.address(),
        tx_hash: receipt_blocks[1].0,
    }];

    assert_next_logs!(stream, expected);

    Ok(())
}
