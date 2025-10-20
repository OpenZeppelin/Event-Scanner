use alloy::providers::ext::AnvilApi;

use crate::common::{TestCounter, increase, setup_scanner};
use event_scanner::{assert_next, test_utils::LogMetadata, types::ScannerStatus};

#[tokio::test]
async fn scan_latest_then_live_happy_path_no_duplicates() -> anyhow::Result<()> {
    let setup = setup_scanner(None, None, None).await?;
    let contract = setup.contract;
    let client = setup.client;
    let mut stream = setup.stream;

    // Historical: produce 6 events total
    _ = increase(&contract).await?;
    _ = increase(&contract).await?;
    _ = increase(&contract).await?;

    let mut expected_latest = vec![];
    expected_latest.push(increase(&contract).await?);
    expected_latest.push(increase(&contract).await?);
    expected_latest.push(increase(&contract).await?);

    // Ask for the latest 3, then live
    client.scan_latest_then_live(3).await?;

    // Latest phase
    assert_next!(stream, expected_latest);
    // Transition to live
    assert_next!(stream, ScannerStatus::SwitchingToLive);

    // Live phase: emit three more, should arrive in order without duplicating latest
    let live1 = increase(&contract).await?;
    let live2 = increase(&contract).await?;

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
    expected_latest.push(increase(&contract).await?);
    expected_latest.push(increase(&contract).await?);

    client.scan_latest_then_live(5).await?;

    // Latest phase returns all available
    assert_next!(stream, &expected_latest);
    assert_next!(stream, ScannerStatus::SwitchingToLive);

    // Live: two more arrive
    let live1 = increase(&contract).await?;
    let live2 = increase(&contract).await?;
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

    // Historical: produce exactly 4 events
    let mut expected_latest = vec![];
    expected_latest.push(increase(&contract).await?);
    expected_latest.push(increase(&contract).await?);
    expected_latest.push(increase(&contract).await?);
    expected_latest.push(increase(&contract).await?);

    client.scan_latest_then_live(4).await?;

    assert_next!(stream, expected_latest);
    assert_next!(stream, ScannerStatus::SwitchingToLive);

    // Live continues
    let live = increase(&contract).await?;
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
    let live1 = increase(&contract).await?;
    let live2 = increase(&contract).await?;
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
    expected_latest.push(increase(&contract).await?);

    provider.anvil_mine(Some(1), None).await?;

    expected_latest.push(increase(&contract).await?);
    expected_latest.push(increase(&contract).await?);

    provider.anvil_mine(Some(1), None).await?;

    client.scan_latest_then_live(3).await?;

    // Latest phase
    assert_next!(stream, &expected_latest);
    assert_next!(stream, ScannerStatus::SwitchingToLive);

    // Immediately produce a new live event in a new block
    let live = increase(&contract).await?;
    assert_next!(stream, &[live]);

    Ok(())
}

#[tokio::test]
async fn scan_latest_then_live_waiting_on_live_logs_arriving() -> anyhow::Result<()> {
    let setup = setup_scanner(None, None, None).await?;
    let contract = setup.contract;
    let client = setup.client;
    let mut stream = setup.stream;

    // Historical: emit 3, mine 1 empty block to form a clear boundary
    let mut expected_latest = vec![];
    expected_latest.push(increase(&contract).await?);
    expected_latest.push(increase(&contract).await?);
    expected_latest.push(increase(&contract).await?);

    client.scan_latest_then_live(3).await?;

    // Latest phase
    assert_next!(stream, &expected_latest);
    assert_next!(stream, ScannerStatus::SwitchingToLive);

    let inner = stream.into_inner();
    assert!(inner.is_empty());

    Ok(())
}
