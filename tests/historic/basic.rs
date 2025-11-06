use alloy::{eips::BlockNumberOrTag, primitives::U256};
use event_scanner::{assert_closed, assert_next};

use crate::common::{TestCounter, setup_historic_scanner};

#[tokio::test]
async fn processes_events_within_specified_historical_range() -> anyhow::Result<()> {
    let setup = setup_historic_scanner(
        Some(0.1),
        None,
        BlockNumberOrTag::Earliest,
        BlockNumberOrTag::Latest,
    )
    .await?;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    contract.increase().send().await?.watch().await?;
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
            TestCounter::CountIncreased { newCount: U256::from(5) },
        ]
    );
    assert_closed!(stream);

    Ok(())
}
