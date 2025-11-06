use alloy::{
    eips::BlockNumberOrTag,
    network::Ethereum,
    primitives::U256,
    providers::{Provider, ext::AnvilApi},
    sol_types::SolEvent,
};

use crate::common::{TestCounter, deploy_counter, setup_common, setup_latest_scanner};
use event_scanner::{EventFilter, EventScannerBuilder, assert_closed, assert_next};

#[tokio::test]
async fn latest_scanner_exact_count_returns_last_events_in_order() -> anyhow::Result<()> {
    let count = 5;
    let setup = setup_latest_scanner(None, None, count, None, None).await?;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    // Produce 8 events
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    // Ask for the latest 5
    scanner.start().await?;

    assert_next!(
        stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(4) },
            TestCounter::CountIncreased { newCount: U256::from(5) },
            TestCounter::CountIncreased { newCount: U256::from(6) },
            TestCounter::CountIncreased { newCount: U256::from(7) },
            TestCounter::CountIncreased { newCount: U256::from(8) },
        ]
    );
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
async fn latest_scanner_fewer_available_than_count_returns_all() -> anyhow::Result<()> {
    let count = 5;
    let setup = setup_latest_scanner(None, None, count, None, None).await?;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    // Produce only 3 events
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    scanner.start().await?;

    assert_next!(
        stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(1) },
            TestCounter::CountIncreased { newCount: U256::from(2) },
            TestCounter::CountIncreased { newCount: U256::from(3) },
        ]
    );
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
async fn latest_scanner_no_events_returns_empty() -> anyhow::Result<()> {
    let count = 5;
    let setup = setup_latest_scanner(None, None, count, None, None).await?;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    scanner.start().await?;

    let expected: &[TestCounter::CountIncreased] = &[];

    assert_next!(stream, expected);
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
async fn latest_scanner_respects_range_subset() -> anyhow::Result<()> {
    let (anvil, provider, contract, default_filter) = setup_common(None, None).await?;
    // Mine 6 events, one per tx (auto-mined), then manually mint 2 empty blocks to widen range
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    // manual empty block minting
    provider.anvil_mine(Some(2), None).await?;

    let head = provider.get_block_number().await?;
    // Choose a subrange covering last 4 blocks
    let start = BlockNumberOrTag::from(head - 3);
    let end = BlockNumberOrTag::from(head);

    let mut scanner_with_range = EventScannerBuilder::latest(10)
        .from_block(start)
        .to_block(end)
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;
    let mut stream_with_range = scanner_with_range.subscribe(default_filter);

    scanner_with_range.start().await?;

    assert_next!(
        stream_with_range,
        &[
            TestCounter::CountIncreased { newCount: U256::from(5) },
            TestCounter::CountIncreased { newCount: U256::from(6) },
        ]
    );
    assert_closed!(stream_with_range);

    Ok(())
}

#[tokio::test]
async fn latest_scanner_multiple_listeners_to_same_event_receive_same_results() -> anyhow::Result<()>
{
    let count = 5;
    let setup = setup_latest_scanner(None, None, count, None, None).await?;
    let contract = setup.contract;
    let mut scanner = setup.scanner;
    let mut stream1 = setup.stream;

    // Add a second listener with the same filter
    let filter2 = EventFilter::new()
        .contract_address(*contract.address())
        .event(TestCounter::CountIncreased::SIGNATURE);
    let mut stream2 = scanner.subscribe(filter2);

    // Produce 7 events
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    scanner.start().await?;

    let expected = &[
        TestCounter::CountIncreased { newCount: U256::from(3) },
        TestCounter::CountIncreased { newCount: U256::from(4) },
        TestCounter::CountIncreased { newCount: U256::from(5) },
        TestCounter::CountIncreased { newCount: U256::from(6) },
        TestCounter::CountIncreased { newCount: U256::from(7) },
    ];

    assert_next!(stream1, expected);
    assert_closed!(stream1);

    assert_next!(stream2, expected);
    assert_closed!(stream2);

    Ok(())
}

#[tokio::test]
async fn latest_scanner_different_filters_receive_different_results() -> anyhow::Result<()> {
    let count = 3;
    let setup = setup_latest_scanner(None, None, count, None, None).await?;
    let contract = setup.contract;
    let mut scanner = setup.scanner;

    // First listener for CountIncreased
    let filter_inc = EventFilter::new()
        .contract_address(*contract.address())
        .event(TestCounter::CountIncreased::SIGNATURE);
    let mut stream_inc = scanner.subscribe(filter_inc);

    // Second listener for CountDecreased
    let filter_dec = EventFilter::new()
        .contract_address(*contract.address())
        .event(TestCounter::CountDecreased::SIGNATURE);
    let mut stream_dec = scanner.subscribe(filter_dec);

    // Produce 5 increases, then 2 decreases
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    contract.decrease().send().await?.watch().await?;
    contract.decrease().send().await?.watch().await?;

    // Ask for latest 3 across the full range: each filtered listener should receive their own last
    // 3 events
    scanner.start().await?;

    assert_next!(
        stream_inc,
        &[
            TestCounter::CountIncreased { newCount: U256::from(3) },
            TestCounter::CountIncreased { newCount: U256::from(4) },
            TestCounter::CountIncreased { newCount: U256::from(5) },
        ]
    );
    assert_closed!(stream_inc);

    assert_next!(
        stream_dec,
        &[
            TestCounter::CountDecreased { newCount: U256::from(4) },
            TestCounter::CountDecreased { newCount: U256::from(3) },
        ]
    );
    assert_closed!(stream_dec);

    Ok(())
}

#[tokio::test]
async fn latest_scanner_mixed_events_and_filters_return_correct_streams() -> anyhow::Result<()> {
    let count = 2;
    let setup = setup_latest_scanner(None, None, count, None, None).await?;
    let contract = setup.contract;
    let mut scanner = setup.scanner;
    let mut stream_inc = setup.stream; // CountIncreased by default

    // Add a CountDecreased listener
    let filter_dec = EventFilter::new()
        .contract_address(*contract.address())
        .event(TestCounter::CountDecreased::SIGNATURE);
    let mut stream_dec = scanner.subscribe(filter_dec);

    // Sequence: inc(1), inc(2), dec(1), inc(2), dec(1)
    // inc -> 1
    contract.increase().send().await?.watch().await?;
    // inc -> 2
    contract.increase().send().await?.watch().await?;
    // dec -> 1
    contract.decrease().send().await?.watch().await?;
    // inc -> 2
    contract.increase().send().await?.watch().await?;
    // dec -> 1
    contract.decrease().send().await?.watch().await?;

    scanner.start().await?;

    assert_next!(
        stream_inc,
        &[
            TestCounter::CountIncreased { newCount: U256::from(2) },
            TestCounter::CountIncreased { newCount: U256::from(2) },
        ]
    );
    assert_closed!(stream_inc);

    assert_next!(
        stream_dec,
        &[
            TestCounter::CountDecreased { newCount: U256::from(1) },
            TestCounter::CountDecreased { newCount: U256::from(1) },
        ]
    );
    assert_closed!(stream_dec);

    Ok(())
}

#[tokio::test]
async fn latest_scanner_cross_contract_filtering() -> anyhow::Result<()> {
    // Manual setup to deploy two contracts
    let count = 5;
    let setup = setup_latest_scanner(None, None, count, None, None).await?;
    let provider = setup.provider;
    let mut scanner = setup.scanner;

    let contract_a = deploy_counter(provider.clone()).await?;
    let contract_b = deploy_counter(provider.clone()).await?;

    // Listener only for contract A CountIncreased
    let filter_a = EventFilter::new()
        .contract_address(*contract_a.address())
        .event(TestCounter::CountIncreased::SIGNATURE);

    let mut stream_a = scanner.subscribe(filter_a);

    // Emit interleaved events from A and B: A(1), B(1), A(2), B(2), A(3)
    contract_a.increase().send().await?.watch().await?;
    contract_b.increase().send().await?.watch().await?; // ignored by filter
    contract_a.increase().send().await?.watch().await?;
    contract_b.increase().send().await?.watch().await?; // ignored by filter
    contract_a.increase().send().await?.watch().await?;

    scanner.start().await?;

    assert_next!(
        stream_a,
        &[
            TestCounter::CountIncreased { newCount: U256::from(1) },
            TestCounter::CountIncreased { newCount: U256::from(2) },
            TestCounter::CountIncreased { newCount: U256::from(3) },
        ]
    );
    assert_closed!(stream_a);

    Ok(())
}

#[tokio::test]
async fn latest_scanner_large_gaps_and_empty_ranges() -> anyhow::Result<()> {
    // Manual setup to mine empty blocks
    let (anvil, provider, contract, default_filter) = setup_common(None, None).await?;

    // Emit 2 events
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    // Mine 10 empty blocks
    provider.anvil_mine(Some(10), None).await?;
    // Emit 1 more event
    contract.increase().send().await?.watch().await?;

    let head = provider.get_block_number().await?;
    let start = BlockNumberOrTag::from(head - 12);
    let end = BlockNumberOrTag::from(head);

    let mut scanner_with_range = EventScannerBuilder::latest(5)
        .from_block(start)
        .to_block(end)
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;
    let mut stream_with_range = scanner_with_range.subscribe(default_filter);

    scanner_with_range.start().await?;

    assert_next!(
        stream_with_range,
        &[
            TestCounter::CountIncreased { newCount: U256::from(1) },
            TestCounter::CountIncreased { newCount: U256::from(2) },
            TestCounter::CountIncreased { newCount: U256::from(3) },
        ]
    );
    assert_closed!(stream_with_range);

    Ok(())
}

#[tokio::test]
async fn latest_scanner_boundary_range_single_block() -> anyhow::Result<()> {
    let (anvil, _provider, contract, default_filter) = setup_common(None, None).await?;

    contract.increase().send().await?.watch().await?;
    let receipt = contract.increase().send().await?.get_receipt().await?;
    contract.increase().send().await?.watch().await?;

    // Pick the expected tx's block number as the block range
    let start = BlockNumberOrTag::from(receipt.block_number.unwrap());
    let end = start;

    let mut scanner_with_range = EventScannerBuilder::latest(5)
        .from_block(start)
        .to_block(end)
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;
    let mut stream_with_range = scanner_with_range.subscribe(default_filter);

    scanner_with_range.start().await?;

    assert_next!(stream_with_range, &[TestCounter::CountIncreased { newCount: U256::from(2) }]);
    assert_closed!(stream_with_range);

    Ok(())
}
