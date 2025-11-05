use anyhow::Ok;

use crate::common::{LiveScannerSetup, TestCounter::CountIncreased, setup_live_scanner};
use alloy::{
    primitives::U256,
    providers::ext::AnvilApi,
    rpc::types::anvil::{ReorgOptions, TransactionData},
};
use event_scanner::{ScannerStatus, assert_empty, assert_next};

#[tokio::test]
async fn reorg_rescans_events_within_same_block() -> anyhow::Result<()> {
    let LiveScannerSetup { provider, contract, scanner, mut stream, anvil: _anvil } =
        setup_live_scanner(None, None, 0).await?;

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
    let tx_block_pairs = vec![
        (TransactionData::JSON(contract.increase().into_transaction_request()), 0),
        (TransactionData::JSON(contract.increase().into_transaction_request()), 0),
        (TransactionData::JSON(contract.increase().into_transaction_request()), 0),
    ];
    provider.anvil_reorg(ReorgOptions { depth: 4, tx_block_pairs }).await?;

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
    let LiveScannerSetup { provider, contract, scanner, mut stream, anvil: _anvil } =
        setup_live_scanner(None, None, 0).await?;

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
    let tx_block_pairs = vec![
        (TransactionData::JSON(contract.increase().into_transaction_request()), 0),
        (TransactionData::JSON(contract.increase().into_transaction_request()), 1),
        (TransactionData::JSON(contract.increase().into_transaction_request()), 2),
    ];
    provider.anvil_reorg(ReorgOptions { depth: 4, tx_block_pairs }).await?;

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
    let LiveScannerSetup { provider, contract, scanner, mut stream, anvil: _anvil } =
        setup_live_scanner(None, None, 0).await?;

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

    // reorg the chain
    let tx_block_pairs =
        vec![(TransactionData::JSON(contract.increase().into_transaction_request()), 0)];
    provider.anvil_reorg(ReorgOptions { depth: 1, tx_block_pairs }).await?;

    // assert expected messages post-reorg
    assert_next!(stream, ScannerStatus::ReorgDetected);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(4) }]);
    assert_empty!(stream);

    Ok(())
}

#[tokio::test]
async fn reorg_depth_two() -> anyhow::Result<()> {
    let LiveScannerSetup { provider, contract, scanner, mut stream, anvil: _anvil } =
        setup_live_scanner(None, None, 0).await?;

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

    // reorg the chain
    let tx_block_pairs =
        vec![(TransactionData::JSON(contract.increase().into_transaction_request()), 0)];
    provider.anvil_reorg(ReorgOptions { depth: 2, tx_block_pairs }).await?;

    // assert expected messages post-reorg
    assert_next!(stream, ScannerStatus::ReorgDetected);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(3) }]);
    assert_empty!(stream);

    Ok(())
}

#[tokio::test]
async fn block_confirmations_mitigate_reorgs() -> anyhow::Result<()> {
    // any reorg ≤ 5 should be invisible to consumers
    let LiveScannerSetup { provider, contract, scanner, stream, anvil: _anvil } =
        setup_live_scanner(None, None, 5).await?;

    scanner.start().await?;

    // mine some initial blocks
    provider.anvil_mine(Some(10), None).await?;

    // emit initial events
    for _ in 0..4 {
        contract.increase().send().await?.watch().await?;
    }

    // assert no events have yet been streamed (since none have sufficient confirmations)
    let stream = assert_empty!(stream);

    // reorg the chain
    // note: we include new txs in the same post-reorg block to showcase that the scanner
    // only streams the post-reorg, confirmed logs
    let tx_block_pairs = vec![
        (TransactionData::JSON(contract.increase().into_transaction_request()), 0),
        (TransactionData::JSON(contract.increase().into_transaction_request()), 0),
    ];
    provider.anvil_reorg(ReorgOptions { depth: 2, tx_block_pairs }).await?;

    // assert that still no events have been streamed
    let mut stream = assert_empty!(stream);

    // mine some additional post-reorg blocks to confirm previous blocks with logs
    provider.anvil_mine(Some(10), None).await?;

    // no `ReorgDetected` should be emitted
    assert_next!(stream, &[CountIncreased { newCount: U256::from(1) }]);
    assert_next!(stream, &[CountIncreased { newCount: U256::from(2) }]);
    assert_next!(
        stream,
        &[CountIncreased { newCount: U256::from(3) }, CountIncreased { newCount: U256::from(4) }]
    );
    assert_empty!(stream);

    Ok(())
}
