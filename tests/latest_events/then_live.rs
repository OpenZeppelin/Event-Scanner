use std::sync::Arc;

use alloy::{
    eips::BlockNumberOrTag,
    primitives::U256,
    providers::{Provider, ext::AnvilApi},
    sol_types::SolEvent,
};

use crate::common::{TestCounter, deploy_counter, setup_scanner};
use event_scanner::{EventFilter, assert_next, test_utils::LogMetadata, types::ScannerStatus};

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

#[tokio::test]
async fn scan_latest_then_live_happy_path_no_duplicates() -> anyhow::Result<()> {
    let setup = setup_scanner(None, None, None).await?;
    let contract = setup.contract;
    let client = setup.client;
    let mut stream = setup.stream;

    // Historical: produce 6 events total
    _ = increase!(contract);
    _ = increase!(contract);
    _ = increase!(contract);

    let mut expected_latest = vec![];
    expected_latest.push(increase!(contract));
    expected_latest.push(increase!(contract));
    expected_latest.push(increase!(contract));

    // Ask for the latest 3, then live
    client.scan_latest_then_live(3).await?;

    // Latest phase
    assert_next!(stream, expected_latest);
    // Transition to live
    assert_next!(stream, ScannerStatus::SwitchingToLive);

    // Live phase: emit three more, should arrive in order without duplicating latest
    let live1 = increase!(contract);
    let live2 = increase!(contract);

    assert_next!(stream, &[live1]);
    assert_next!(stream, &[live2]);

    Ok(())
}

#[tokio::test]
async fn scan_latest_then_live_fewer_historical_then_continues_live() -> anyhow::Result<()> {
    let setup = setup_scanner(None, None, None).await?;
    let contract = setup.contract;
    let client = setup.client;
    let mut stream = setup.stream;

    // Historical: only 2 available
    let mut expected_latest = vec![];
    expected_latest.push(increase!(contract));
    expected_latest.push(increase!(contract));

    client.scan_latest_then_live(5).await?;

    // Latest phase returns all available
    assert_next!(stream, &expected_latest);
    assert_next!(stream, ScannerStatus::SwitchingToLive);

    // Live: two more arrive
    let live1 = increase!(contract);
    let live2 = increase!(contract);
    assert_next!(stream, &[live1]);
    assert_next!(stream, &[live2]);

    Ok(())
}

#[tokio::test]
async fn scan_latest_then_live_exact_historical_count_then_live() -> anyhow::Result<()> {
    let setup = setup_scanner(None, None, None).await?;
    let contract = setup.contract;
    let client = setup.client;
    let mut stream = setup.stream;

    // Historical: produce exactly 4 across two phases
    _ = increase!(contract);
    let expected_latest = &[increase!(contract), increase!(contract), increase!(contract)];

    client.scan_latest_then_live(4).await?;

    assert_next!(stream, expected_latest);
    assert_next!(stream, ScannerStatus::SwitchingToLive);

    // Live continues
    let live = increase!(contract);
    assert_next!(stream, &[live]);

    Ok(())
}

#[tokio::test]
async fn scan_latest_then_live_no_historical_only_live_streams() -> anyhow::Result<()> {
    let setup = setup_scanner(None, None, None).await?;
    let contract = setup.contract;
    let client = setup.client;
    let mut stream = setup.stream;

    client.scan_latest_then_live(5).await?;

    // Latest is empty
    let expected: &[LogMetadata<TestCounter::CountIncreased>] = &[];
    assert_next!(stream, expected);
    assert_next!(stream, ScannerStatus::SwitchingToLive);

    // Live events arrive
    let live1 = increase!(contract);
    let live2 = increase!(contract);
    assert_next!(stream, &[live1]);
    assert_next!(stream, &[live2]);

    Ok(())
}

#[tokio::test]
async fn scan_latest_then_live_boundary_no_duplication() -> anyhow::Result<()> {
    let setup = setup_scanner(None, None, None).await?;
    let provider = setup.provider;
    let contract = setup.contract;
    let client = setup.client;
    let mut stream = setup.stream;

    // Historical: emit 3, mine 1 empty block to form a clear boundary
    let mut expected_latest = vec![];
    expected_latest.push(increase!(contract));
    expected_latest.push(increase!(contract));
    expected_latest.push(increase!(contract));

    provider.anvil_mine(Some(1), None).await?;

    client.scan_latest_then_live(3).await?;

    // Latest phase
    assert_next!(stream, &expected_latest);
    assert_next!(stream, ScannerStatus::SwitchingToLive);

    // Immediately produce a new live event in a new block
    let live = increase!(contract);
    assert_next!(stream, &[live]);

    Ok(())
}
