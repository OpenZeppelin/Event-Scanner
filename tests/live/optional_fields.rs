use crate::common::{TestCounter, deploy_counter, setup_live_scanner};
use alloy::primitives::U256;
use event_scanner::{EventFilter, assert_empty, assert_event_sequence_final, assert_next};

#[tokio::test]
async fn track_all_events_from_contract() -> anyhow::Result<()> {
    let setup = setup_live_scanner(None, None, 0).await?;
    let contract = setup.contract;
    let contract_address = *contract.address();

    let mut scanner = setup.scanner;

    // Create filter that tracks ALL events from a specific contract (no event signature specified)
    let filter = EventFilter::new().contract_address(contract_address);

    let mut stream = scanner.subscribe(filter);

    scanner.start().await?;

    // Generate both increase and decrease events
    for _ in 0..5 {
        contract.increase().send().await?.watch().await?;
    }

    // Also generate some decrease events to ensure we're tracking all events
    contract.decrease().send().await?.watch().await?;
    contract.decrease().send().await?.watch().await?;

    assert_event_sequence_final!(
        stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(1) },
            TestCounter::CountIncreased { newCount: U256::from(2) },
            TestCounter::CountIncreased { newCount: U256::from(3) },
            TestCounter::CountIncreased { newCount: U256::from(4) },
            TestCounter::CountIncreased { newCount: U256::from(5) },
            TestCounter::CountDecreased { newCount: U256::from(4) },
            TestCounter::CountDecreased { newCount: U256::from(3) }
        ]
    );

    Ok(())
}

#[tokio::test]
async fn track_all_events_in_block_range() -> anyhow::Result<()> {
    let setup = setup_live_scanner(Some(0.1), None, 0).await?;
    let contract = setup.contract;
    let mut scanner = setup.scanner;

    // Create filter that tracks ALL events in block range (no contract address or event signature
    // specified)
    let filter = EventFilter::new();

    let mut stream = scanner.subscribe(filter);

    scanner.start().await?;

    // Generate events from our contract
    for _ in 0..3 {
        contract.increase().send().await?.watch().await?;
    }

    assert_event_sequence_final!(
        stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(1) },
            TestCounter::CountIncreased { newCount: U256::from(2) },
            TestCounter::CountIncreased { newCount: U256::from(3) },
        ]
    );

    Ok(())
}

#[tokio::test]
async fn mixed_optional_and_required_filters() -> anyhow::Result<()> {
    let setup = setup_live_scanner(None, None, 0).await?;
    let contract_1 = setup.contract;
    let contract_2 = deploy_counter(setup.provider.primary().clone()).await?;
    let mut scanner = setup.scanner;

    // Filter for all events from all contracts
    let all_events_filter = EventFilter::new();
    let mut all_stream = scanner.subscribe(all_events_filter);
    let contract_1_stream = setup.stream;

    scanner.start().await?;

    // First increase the counter to have some balance
    contract_2.increase().send().await?.watch().await?;
    contract_2.increase().send().await?.watch().await?;
    contract_2.increase().send().await?.watch().await?;

    let mut all_stream = assert_event_sequence_final!(
        all_stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(1) },
            TestCounter::CountIncreased { newCount: U256::from(2) },
            TestCounter::CountIncreased { newCount: U256::from(3) }
        ]
    );

    let mut contract_1_stream = assert_empty!(contract_1_stream);

    // Generate events from contract 1 (CountIncreased)
    contract_1.increase().send().await?.watch().await?;
    contract_1.increase().send().await?.watch().await?;

    let mut all_stream = assert_event_sequence_final!(
        all_stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(1) },
            TestCounter::CountIncreased { newCount: U256::from(2) }
        ]
    );
    let contract_1_stream = assert_event_sequence_final!(
        contract_1_stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(1) },
            TestCounter::CountIncreased { newCount: U256::from(2) }
        ]
    );

    // Generate additional events that should be caught by the all-events filter
    contract_2.decrease().send().await?.watch().await?;

    assert_next!(all_stream, &[TestCounter::CountDecreased { newCount: U256::from(2) }]);

    assert_empty!(all_stream);
    assert_empty!(contract_1_stream);

    Ok(())
}
