use alloy::{primitives::U256, providers::ext::AnvilApi};

use crate::common::{TestCounter, setup_sync_from_latest_scanner};
use event_scanner::{ScannerStatus, assert_next};

#[tokio::test]
async fn scan_latest_then_live_happy_path_no_duplicates() -> anyhow::Result<()> {
    let setup = setup_sync_from_latest_scanner(None, None, 3, 0).await?;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    // Historical: produce 6 events total
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    // Ask for the latest 3, then live
    scanner.start().await?;

    // Latest phase
    assert_next!(
        stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(4) },
            TestCounter::CountIncreased { newCount: U256::from(5) },
            TestCounter::CountIncreased { newCount: U256::from(6) },
        ]
    );
    // Transition to live
    assert_next!(stream, ScannerStatus::SwitchingToLive);

    // Live phase: emit three more, should arrive in order without duplicating latest
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(7) }]);
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(8) }]);

    Ok(())
}

#[tokio::test]
async fn scan_latest_then_live_fewer_historical_then_continues_live() -> anyhow::Result<()> {
    let setup = setup_sync_from_latest_scanner(None, None, 5, 0).await?;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    // Historical: only 2 available
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    scanner.start().await?;

    // Latest phase returns all available
    assert_next!(
        stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(1) },
            TestCounter::CountIncreased { newCount: U256::from(2) },
        ]
    );
    assert_next!(stream, ScannerStatus::SwitchingToLive);

    // Live: two more arrive
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(3) }]);
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(4) }]);

    Ok(())
}

#[tokio::test]
async fn scan_latest_then_live_exact_historical_count_then_live() -> anyhow::Result<()> {
    let setup = setup_sync_from_latest_scanner(None, None, 4, 0).await?;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    // Historical: produce exactly 4 events
    contract.increase().send().await?.watch().await?;
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
            TestCounter::CountIncreased { newCount: U256::from(4) },
        ]
    );
    assert_next!(stream, ScannerStatus::SwitchingToLive);

    // Live continues
    contract.increase().send().await?.watch().await?;
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(5) }]);

    Ok(())
}

#[tokio::test]
async fn scan_latest_then_live_no_historical_only_live_streams() -> anyhow::Result<()> {
    let setup = setup_sync_from_latest_scanner(None, None, 5, 0).await?;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    scanner.start().await?;

    // Latest is empty
    let expected: &[TestCounter::CountIncreased] = &[];
    assert_next!(stream, expected);
    assert_next!(stream, ScannerStatus::SwitchingToLive);

    // Live events arrive
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(1) }]);
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(2) }]);

    Ok(())
}

#[tokio::test]
async fn scan_latest_then_live_boundary_no_duplication() -> anyhow::Result<()> {
    let setup = setup_sync_from_latest_scanner(None, None, 3, 0).await?;
    let provider = setup.provider;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    // Historical: emit 3, mine 1 empty block to form a clear boundary
    contract.increase().send().await?.watch().await?;

    provider.primary().anvil_mine(Some(1), None).await?;

    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    provider.primary().anvil_mine(Some(1), None).await?;

    scanner.start().await?;

    // Latest phase
    assert_next!(
        stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(1) },
            TestCounter::CountIncreased { newCount: U256::from(2) },
            TestCounter::CountIncreased { newCount: U256::from(3) },
        ]
    );
    assert_next!(stream, ScannerStatus::SwitchingToLive);

    // Immediately produce a new live event in a new block
    contract.increase().send().await?.watch().await?;
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(4) }]);

    Ok(())
}

#[tokio::test]
async fn scan_latest_then_live_waiting_on_live_logs_arriving() -> anyhow::Result<()> {
    let setup = setup_sync_from_latest_scanner(None, None, 3, 0).await?;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    // Historical: emit 3, mine 1 empty block to form a clear boundary
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    scanner.start().await?;

    // Latest phase
    assert_next!(
        stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(1) },
            TestCounter::CountIncreased { newCount: U256::from(2) },
            TestCounter::CountIncreased { newCount: U256::from(3) },
        ]
    );
    assert_next!(stream, ScannerStatus::SwitchingToLive);

    let inner = stream.into_inner();
    assert!(inner.is_empty());

    Ok(())
}
