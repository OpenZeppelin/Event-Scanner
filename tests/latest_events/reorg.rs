use alloy::{primitives::U256, providers::ext::AnvilApi, rpc::types::anvil::ReorgOptions};

use crate::common::{TestCounter::CountIncreased, setup_common, setup_latest_scanner};
use event_scanner::{EventScannerBuilder, assert_closed, assert_next};

#[tokio::test]
#[ignore = "Currently relies on a race condition - will be fixed with https://github.com/OpenZeppelin/Event-Scanner/issues/218"]
async fn reorged_logs_are_removed_from_stream() -> anyhow::Result<()> {
    let (_anvil, provider, contract, filter) = setup_common(None, None).await?;

    for _ in 0..10 {
        contract.increase().send().await?.watch().await?;
    }

    let mut scanner =
        EventScannerBuilder::latest(5).max_block_range(2).connect(provider.clone()).await?;
    let mut stream = scanner.subscribe(filter);

    scanner.start().await?;

    // Trigger a reorg that removes the last 2 blocks (events 19 and 20)
    provider.primary().anvil_reorg(ReorgOptions { depth: 2, tx_block_pairs: vec![] }).await?;

    // After reorg, 18 events remain (1-18), latest 5 = events 14-18
    assert_next!(
        stream,
        &[
            CountIncreased { newCount: U256::from(4) },
            CountIncreased { newCount: U256::from(5) },
            CountIncreased { newCount: U256::from(6) },
            CountIncreased { newCount: U256::from(7) },
            CountIncreased { newCount: U256::from(8) },
        ]
    );
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
#[ignore = "Currently relies on a race condition - will be fixed with https://github.com/OpenZeppelin/Event-Scanner/issues/218"]
async fn new_logs_in_reorged_blocks_are_included() -> anyhow::Result<()> {
    use alloy::rpc::types::anvil::TransactionData;

    let setup = setup_latest_scanner(None, None, 5, None, None).await?;
    let provider = setup.provider;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    for _ in 0..10 {
        contract.increase().send().await?.watch().await?;
    }

    scanner.start().await?;

    // Trigger a reorg that removes 2 blocks  and adds 2 new events in replacement blocks.
    let tx_block_pairs = vec![
        (TransactionData::JSON(contract.increase().into_transaction_request()), 0),
        (TransactionData::JSON(contract.increase().into_transaction_request()), 1),
    ];

    provider.primary().anvil_reorg(ReorgOptions { depth: 2, tx_block_pairs }).await?;

    // NOTE: 9 and 10 are from post reorg block (however can verify this in test)
    // Should be correct given previous test assertion
    assert_next!(
        stream,
        &[
            CountIncreased { newCount: U256::from(6) },
            CountIncreased { newCount: U256::from(7) },
            CountIncreased { newCount: U256::from(8) },
            CountIncreased { newCount: U256::from(9) },
            CountIncreased { newCount: U256::from(10) },
        ]
    );
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
#[ignore = "Currently relies on a race condition - will be fixed with https://github.com/OpenZeppelin/Event-Scanner/issues/218"]
async fn rewind_continues_further_when_reorg_removes_logs() -> anyhow::Result<()> {
    let setup = setup_latest_scanner(None, None, 5, None, None).await?;
    let provider = setup.provider;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    for _ in 0..10 {
        contract.increase().send().await?.watch().await?;
    }

    scanner.start().await?;

    provider.primary().anvil_reorg(ReorgOptions { depth: 3, tx_block_pairs: vec![] }).await?;

    // Originally would collect events 6-10, but 8-10 are reorged away
    // Scanner continues rewinding and should collect 5 valid events: 3-7
    assert_next!(
        stream,
        &[
            CountIncreased { newCount: U256::from(3) },
            CountIncreased { newCount: U256::from(4) },
            CountIncreased { newCount: U256::from(5) },
            CountIncreased { newCount: U256::from(6) },
            CountIncreased { newCount: U256::from(7) },
        ]
    );
    assert_closed!(stream);

    Ok(())
}

#[tokio::test]
#[ignore = "Currently relies on a race condition - will be fixed with https://github.com/OpenZeppelin/Event-Scanner/issues/218"]
async fn deep_reorg_removes_exhaust_requested_count() -> anyhow::Result<()> {
    let setup = setup_latest_scanner(None, None, 5, None, None).await?;
    let provider = setup.provider;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    for _ in 0..8 {
        contract.increase().send().await?.watch().await?;
    }

    scanner.start().await?;

    // Reorg removes 5 blocks - this removes events 4-8
    // Only events 1-3 remain
    provider.primary().anvil_reorg(ReorgOptions { depth: 5, tx_block_pairs: vec![] }).await?;

    assert_next!(
        stream,
        &[
            CountIncreased { newCount: U256::from(1) },
            CountIncreased { newCount: U256::from(2) },
            CountIncreased { newCount: U256::from(3) },
        ]
    );
    assert_closed!(stream);

    Ok(())
}
