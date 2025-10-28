use alloy::primitives::U256;
use event_scanner::{assert_next, types::ScannerStatus};

use crate::common::{TestCounter, setup_sync_scanner};

#[tokio::test]
async fn replays_historical_then_switches_to_live() -> anyhow::Result<()> {
    let setup = setup_sync_scanner(Some(0.1), None, 0).await?;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    scanner.start().await?;

    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    // historical events
    assert_next!(
        stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(1) },
            TestCounter::CountIncreased { newCount: U256::from(2) },
            TestCounter::CountIncreased { newCount: U256::from(3) },
        ]
    );

    // chain tip reached
    assert_next!(stream, ScannerStatus::ChainTipReached);

    // live events
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(4) },]);
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(5) },]);

    Ok(())
}
