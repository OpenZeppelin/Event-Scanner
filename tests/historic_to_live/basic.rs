use alloy::{eips::BlockNumberOrTag, primitives::U256};
use event_scanner::{assert_next, types::ScannerStatus};

use crate::common::{TestCounter, setup_scanner};

#[tokio::test]
async fn replays_historical_then_switches_to_live() -> anyhow::Result<()> {
    let setup = setup_scanner(None, None, None).await?;
    let contract = setup.contract;
    let client = setup.client;
    let mut stream = setup.stream;

    let receipt = contract.increase().send().await?.get_receipt().await?;
    let first_historical_block =
        receipt.block_number.expect("historical receipt should contain block number");

    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    client.start_scanner(BlockNumberOrTag::Number(first_historical_block), None).await?;

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
