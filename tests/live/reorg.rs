use anyhow::Ok;

use crate::common::{TestCounter::CountIncreased, setup_common};
use alloy::{
    primitives::U256,
    providers::ext::AnvilApi,
    rpc::types::anvil::{ReorgOptions, TransactionData},
};
use event_scanner::{EventScannerBuilder, ScannerStatus, assert_empty, assert_next};

#[tokio::test]
async fn reorg_rescans_events_within_same_block() -> anyhow::Result<()> {
    let (_anvil, provider, contract, filter) = setup_common(None, None).await?;
    let mut scanner = EventScannerBuilder::live().connect(provider.clone());
    let mut stream = scanner.subscribe(filter);

    scanner.start().await?;

    // emit initial events
    for _ in 0..5 {
        contract.increase().send().await?.watch().await?;
    }

    // assert initial events are emitted as expected
    assert_next!(stream, &[CountIncreased { newCount: U256::from(1) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(2) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(3) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(4) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(5) }]);
    let mut stream = assert_empty!(stream);

    // reorg the chain
    let tx_block_pairs = (0..3)
        .map(|_| (TransactionData::JSON(contract.increase().into_transaction_request()), 0))
        .collect();

    provider.inner().anvil_reorg(ReorgOptions { depth: 4, tx_block_pairs }).await?;

    // assert expected messages post-reorg
    assert_next!(stream, ScannerStatus::ReorgDetected);
    assert_next!(
        stream,
        &[
            CountIncreased { newCount: U256::from(2) },
            CountIncreased { newCount: U256::from(3) },
            CountIncreased { newCount: U256::from(4) },
        ]
    );
    assert_empty!(stream);

    Ok(())
}

#[tokio::test]
async fn reorg_rescans_events_with_ascending_blocks() -> anyhow::Result<()> {
    let (_anvil, provider, contract, filter) = setup_common(None, None).await?;
    let mut scanner = EventScannerBuilder::live().connect(provider.clone());
    let mut stream = scanner.subscribe(filter);

    scanner.start().await?;

    // emit initial events
    for _ in 0..5 {
        contract.increase().send().await?.watch().await?;
    }

    // assert initial events are emitted as expected
    assert_next!(stream, &[CountIncreased { newCount: U256::from(1) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(2) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(3) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(4) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(5) }]);
    let mut stream = assert_empty!(stream);

    // reorg the chain - new events in ascending blocks
    let tx_block_pairs = vec![
        (TransactionData::JSON(contract.increase().into_transaction_request()), 0),
        (TransactionData::JSON(contract.increase().into_transaction_request()), 1),
        (TransactionData::JSON(contract.increase().into_transaction_request()), 2),
    ];

    provider.inner().anvil_reorg(ReorgOptions { depth: 4, tx_block_pairs }).await?;

    // assert expected messages post-reorg
    assert_next!(stream, ScannerStatus::ReorgDetected);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(2) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(3) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(4) }]);
    assert_empty!(stream);

    Ok(())
}

#[tokio::test]
async fn reorg_depth_one() -> anyhow::Result<()> {
    let (_anvil, provider, contract, filter) = setup_common(None, None).await?;
    let mut scanner = EventScannerBuilder::live().connect(provider.clone());
    let mut stream = scanner.subscribe(filter);

    scanner.start().await?;

    // emit initial events
    for _ in 0..4 {
        contract.increase().send().await?.watch().await?;
    }

    // assert initial events are emitted as expected
    assert_next!(stream, &[CountIncreased { newCount: U256::from(1) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(2) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(3) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(4) }]);
    let mut stream = assert_empty!(stream);

    // reorg the chain with depth 1
    let tx_block_pairs =
        vec![(TransactionData::JSON(contract.increase().into_transaction_request()), 0)];

    provider.inner().anvil_reorg(ReorgOptions { depth: 1, tx_block_pairs }).await?;

    // assert expected messages post-reorg
    assert_next!(stream, ScannerStatus::ReorgDetected);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(4) }]);
    assert_empty!(stream);

    Ok(())
}

#[tokio::test]
async fn reorg_depth_two() -> anyhow::Result<()> {
    let (_anvil, provider, contract, filter) = setup_common(None, None).await?;
    let mut scanner = EventScannerBuilder::live().block_confirmations(0).connect(provider.clone());
    let mut stream = scanner.subscribe(filter);

    scanner.start().await?;

    // emit initial events
    for _ in 0..4 {
        contract.increase().send().await?.watch().await?;
    }

    // assert initial events are emitted as expected
    assert_next!(stream, &[CountIncreased { newCount: U256::from(1) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(2) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(3) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(4) }]);
    let mut stream = assert_empty!(stream);

    // reorg the chain with depth 2
    let tx_block_pairs =
        vec![(TransactionData::JSON(contract.increase().into_transaction_request()), 0)];

    provider.inner().anvil_reorg(ReorgOptions { depth: 2, tx_block_pairs }).await?;

    // assert expected messages post-reorg
    // After reorg depth 2, we rolled back events 3 and 4, so counter is at 2
    // The new event will increment from 2 to 3
    assert_next!(stream, ScannerStatus::ReorgDetected);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(3) }]);
    assert_empty!(stream);

    Ok(())
}

#[tokio::test]
async fn block_confirmations_mitigate_reorgs() -> anyhow::Result<()> {
    // any reorg ≤ 5 should be invisible to consumers
    let block_confirmations = 5;
    let (_anvil, provider, contract, filter) = setup_common(None, None).await?;
    let mut scanner = EventScannerBuilder::live()
        .block_confirmations(block_confirmations)
        .connect(provider.clone());
    let mut stream = scanner.subscribe(filter);

    scanner.start().await?;

    // mine some blocks to establish a baseline
    provider.clone().inner().anvil_mine(Some(10), None).await?;

    // emit initial events
    for _ in 0..4 {
        contract.increase().send().await?.watch().await?;
    }

    // mine enough blocks to confirm first 2 events (but not events 3 and 4)
    // Events are in blocks 11, 12, 13, 14. After mining 7 blocks, we're at block 21.
    // Block 11 needs to reach block 16 for confirmation (5 confirmations)
    // Block 12 needs to reach block 17 for confirmation
    // So after 7 more blocks, events 1 and 2 are confirmed, but not 3 and 4
    provider.clone().inner().anvil_mine(Some(7), None).await?;

    // assert first 2 events are emitted (confirmed)
    assert_next!(stream, &[CountIncreased { newCount: U256::from(1) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(2) }]);
    let mut stream = assert_empty!(stream);

    // Now do a reorg of depth 2 (removes blocks with events 3 and 4, which weren't confirmed yet)
    // Add 2 new events in the reorged chain
    let tx_block_pairs = vec![
        (TransactionData::JSON(contract.increase().into_transaction_request()), 0),
        (TransactionData::JSON(contract.increase().into_transaction_request()), 0),
    ];

    provider.clone().inner().anvil_reorg(ReorgOptions { depth: 2, tx_block_pairs }).await?;

    // mine enough blocks to confirm the new events
    provider.clone().inner().anvil_mine(Some(10), None).await?;

    // assert the new events from the reorged chain
    // No ReorgDetected should be emitted because the reorg happened before confirmation
    assert_next!(stream, &[CountIncreased { newCount: U256::from(3) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(4) }]);
    assert_empty!(stream);

    Ok(())
}
