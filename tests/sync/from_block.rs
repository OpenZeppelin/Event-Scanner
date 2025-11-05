use alloy::{
    eips::BlockNumberOrTag,
    primitives::U256,
    providers::ext::AnvilApi,
    rpc::types::anvil::{ReorgOptions, TransactionData},
};
use event_scanner::{ScannerStatus, assert_empty, assert_next};

use crate::common::{TestCounter, setup_sync_scanner};

#[tokio::test]
async fn replays_historical_then_switches_to_live() -> anyhow::Result<()> {
    let setup = setup_sync_scanner(None, None, BlockNumberOrTag::Earliest, 0).await?;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    // emit "historic" events
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    scanner.start().await?;

    // now emit new events
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
    assert_next!(stream, ScannerStatus::SwitchingToLive);

    // live events
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(4) }]);
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(5) }]);
    assert_empty!(stream);

    Ok(())
}

#[tokio::test]
async fn sync_from_future_block_waits_until_minted() -> anyhow::Result<()> {
    let future_start_block = 4;
    let setup = setup_sync_scanner(None, None, future_start_block, 0).await?;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let stream = setup.stream;

    // Start the scanner in sync mode from the future block
    scanner.start().await?;

    // Send 2 transactions that should not appear in the stream
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    // Assert: no messages should be received before reaching the start height
    let mut stream = assert_empty!(stream);

    // Act: emit an event that will be mined in block == future_start
    contract.increase().send().await?.watch().await?;

    // Assert: the first streamed message arrives and contains the expected event
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(3) }]);
    assert_empty!(stream);

    Ok(())
}

#[test_log::test(tokio::test)]
async fn block_confirmations_mitigate_reorgs() -> anyhow::Result<()> {
    // any reorg ≤ 5 should be invisible to consumers
    let setup = setup_sync_scanner(None, None, BlockNumberOrTag::Earliest, 5).await?;
    let provider = setup.provider;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    // mine some initial "historic" blocks
    contract.increase().send().await?.watch().await?;
    provider.root().anvil_mine(Some(5), None).await?;

    scanner.start().await?;

    // emit "live" events
    for _ in 0..4 {
        contract.increase().send().await?.watch().await?;
    }

    // assert only the first events has enough confirmations to be streamed
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(1) }]);
    assert_next!(stream, ScannerStatus::SwitchingToLive);
    let stream = assert_empty!(stream);

    // Perform a shallow reorg on the live tail
    // note: we include new txs in the same post-reorg block to showcase that the scanner
    // only streams the post-reorg, confirmed logs
    let tx_block_pairs = vec![
        (TransactionData::JSON(contract.increase().into_transaction_request()), 0),
        (TransactionData::JSON(contract.increase().into_transaction_request()), 0),
    ];
    provider.root().anvil_reorg(ReorgOptions { depth: 2, tx_block_pairs }).await?;

    // assert that still no events have been streamed
    let mut stream = assert_empty!(stream);

    // mine some additional post-reorg blocks to confirm previous blocks with logs
    provider.root().anvil_mine(Some(10), None).await?;

    // no `ReorgDetected` should be emitted
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(2) }]);
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(3) }]);
    assert_next!(
        stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(4) },
            TestCounter::CountIncreased { newCount: U256::from(5) }
        ]
    );
    assert_empty!(stream);

    Ok(())
}
