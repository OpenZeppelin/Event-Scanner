use alloy::{
    eips::BlockNumberOrTag,
    primitives::U256,
    providers::ext::AnvilApi,
    rpc::types::anvil::{ReorgOptions, TransactionData},
};
use event_scanner::{ScannerStatus, assert_empty, assert_next};
use tokio_stream::wrappers::ReceiverStream;

use crate::common::{TestCounter, TestCounterExt, setup_sync_scanner};

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
    _ = contract.increase_and_get_meta().await?;
    _ = contract.increase_and_get_meta().await?;

    // Assert: no messages should be received before reaching the start height
    let inner = setup.stream.into_inner();
    assert!(inner.is_empty());
    let mut stream = ReceiverStream::new(inner);

    // Act: emit an event that will be mined in block == future_start
    let expected = &[contract.increase_and_get_meta().await?];

    // Assert: the first streamed message arrives and contains the expected event
    assert_next!(stream, expected);

    Ok(())
}

#[tokio::test]
async fn block_confirmations_mitigate_reorgs_historic_to_live() -> anyhow::Result<()> {
    // any reorg ≤ 5 should be invisible to consumers
    let block_confirmations = 5;

    let setup =
        setup_sync_scanner(Some(1.0), None, BlockNumberOrTag::Earliest, block_confirmations)
            .await?;
    let provider = setup.provider.clone();
    let contract = setup.contract.clone();

    // mine some blocks to establish a baseline
    provider.clone().root().anvil_mine(Some(10), None).await?;

    let scanner = setup.scanner;
    let mut stream = setup.stream;

    scanner.start().await?;

    // emit initial events
    for _ in 0..4 {
        contract.increase().send().await?.watch().await?;
    }

    // mine enough blocks to confirm first 2 events (but not events 3 and 4)
    provider.clone().root().anvil_mine(Some(7), None).await?;

    // assert first 2 events are emitted (confirmed)
    assert_next!(stream, ScannerStatus::SwitchingToLive);
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(1) }]);
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(2) }]);
    let mut stream = assert_empty!(stream);

    // Now do a reorg of depth 2 (removes blocks with events 3 and 4, which weren't confirmed yet)
    // Add 2 new events in the reorged chain in separate blocks
    let tx_block_pairs = vec![
        (TransactionData::JSON(contract.increase().into_transaction_request()), 0),
        (TransactionData::JSON(contract.increase().into_transaction_request()), 1),
    ];

    provider.clone().root().anvil_reorg(ReorgOptions { depth: 2, tx_block_pairs }).await?;

    // mine enough blocks to confirm the new events
    provider.clone().root().anvil_mine(Some(10), None).await?;

    // assert the new events from the reorged chain
    // No ReorgDetected should be emitted because the reorg happened before confirmation
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(3) }]);
    assert_next!(stream, &[TestCounter::CountIncreased { newCount: U256::from(4) }]);
    assert_empty!(stream);

    Ok(())
}
