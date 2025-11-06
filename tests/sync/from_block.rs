use alloy::{
    eips::BlockNumberOrTag,
    primitives::U256,
    providers::ext::AnvilApi,
    rpc::types::anvil::{ReorgOptions, TransactionData},
};
use event_scanner::{ScannerStatus, assert_empty, assert_next};

use crate::common::{
    SyncScannerSetup,
    TestCounter::{self, CountIncreased},
    setup_sync_scanner,
};

#[tokio::test]
async fn replays_historical_then_switches_to_live() -> anyhow::Result<()> {
    let setup = setup_sync_scanner(Some(0.1), None, BlockNumberOrTag::Earliest, 0).await?;
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
    assert_next!(stream, ScannerStatus::SwitchingToLive);

    // live events
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(4) },]);
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(5) },]);

    Ok(())
}

#[tokio::test]
async fn sync_from_future_block_waits_until_minted() -> anyhow::Result<()> {
    let future_start_block = 4;
    let setup = setup_sync_scanner(None, None, future_start_block, 0).await?;
    let contract = setup.contract;
    let scanner = setup.scanner;

    // Start the scanner in sync mode from the future block
    scanner.start().await?;

    // Send 2 transactions that should not appear in the stream
    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    // Assert: no messages should be received before reaching the start height
    let mut stream = assert_empty!(setup.stream);

    // Act: emit an event that will be mined in block == future_start
    contract.increase().send().await?.watch().await?;

    // Assert: the first streamed message arrives and contains the expected event
    assert_next!(stream, &[CountIncreased { newCount: U256::from(3) }]);

    Ok(())
}

#[tokio::test]
async fn block_confirmations_mitigate_reorgs_historic_to_live() -> anyhow::Result<()> {
    // any reorg ≤ 5 should be invisible to consumers
    let SyncScannerSetup { provider, contract, scanner, mut stream, anvil: _anvil } =
        setup_sync_scanner(None, None, BlockNumberOrTag::Earliest, 5).await?;

    // emit initial events
    for _ in 0..10 {
        contract.increase().send().await?.watch().await?;
    }

    scanner.start().await?;

    assert_next!(
        stream,
        &[
            CountIncreased { newCount: U256::from(1) },
            CountIncreased { newCount: U256::from(2) },
            CountIncreased { newCount: U256::from(3) },
            CountIncreased { newCount: U256::from(4) },
            CountIncreased { newCount: U256::from(5) },
        ]
    );
    assert_next!(stream, ScannerStatus::SwitchingToLive);
    let stream = assert_empty!(stream);

    // reorg the chain
    let tx_block_pairs = vec![
        (TransactionData::JSON(contract.increase().into_transaction_request()), 0),
        (TransactionData::JSON(contract.increase().into_transaction_request()), 1),
    ];
    provider.anvil_reorg(ReorgOptions { depth: 2, tx_block_pairs }).await?;

    // assert no events have still been streamed
    let mut stream = assert_empty!(stream);

    // mine some additional post-reorg blocks
    provider.anvil_mine(Some(10), None).await?;

    // no `ReorgDetected` should be emitted
    assert_next!(stream, &[CountIncreased { newCount: U256::from(6) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(7) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(8) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(9) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(10) }]);
    assert_empty!(stream);

    Ok(())
}
