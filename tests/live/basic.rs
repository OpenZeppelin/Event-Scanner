use crate::common::{TestCounter, deploy_counter, setup_live_scanner};
use alloy::{primitives::U256, sol_types::SolEvent};
use event_scanner::{EventFilter, assert_empty, assert_event_sequence_final};

#[tokio::test]
async fn basic_single_event_scanning() -> anyhow::Result<()> {
    let setup = setup_live_scanner(None, None, 0).await?;
    let contract = setup.contract;
    let scanner = setup.scanner;
    let mut stream = setup.stream;

    scanner.start().await?;

    for _ in 0..5 {
        contract.increase().send().await?.watch().await?;
    }

    assert_event_sequence_final!(
        stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(1) },
            TestCounter::CountIncreased { newCount: U256::from(2) },
            TestCounter::CountIncreased { newCount: U256::from(3) },
            TestCounter::CountIncreased { newCount: U256::from(4) },
            TestCounter::CountIncreased { newCount: U256::from(5) }
        ]
    );

    Ok(())
}

#[tokio::test]
async fn multiple_contracts_same_event_isolate_callbacks() -> anyhow::Result<()> {
    let setup = setup_live_scanner(None, None, 0).await?;
    let provider = setup.provider;
    let mut scanner = setup.scanner;

    let a = setup.contract;
    let mut a_stream = setup.stream;

    let b = deploy_counter(provider.primary().clone()).await?;
    let b_filter = EventFilter::new()
        .contract_address(*b.address())
        .event(TestCounter::CountIncreased::SIGNATURE.to_owned());
    let mut b_stream = scanner.subscribe(b_filter);

    scanner.start().await?;

    for _ in 0..3 {
        a.increase().send().await?.watch().await?;
    }

    for _ in 0..2 {
        b.increase().send().await?.watch().await?;
    }

    assert_event_sequence_final!(
        a_stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(1) },
            TestCounter::CountIncreased { newCount: U256::from(2) },
            TestCounter::CountIncreased { newCount: U256::from(3) }
        ]
    );
    assert_event_sequence_final!(
        b_stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(1) },
            TestCounter::CountIncreased { newCount: U256::from(2) }
        ]
    );

    Ok(())
}

#[tokio::test]
async fn multiple_events_same_contract() -> anyhow::Result<()> {
    let setup = setup_live_scanner(None, None, 0).await?;
    let mut scanner = setup.scanner;
    let contract = setup.contract;
    let mut incr_stream = setup.stream;

    let decrease_filter = EventFilter::new()
        .contract_address(*contract.address())
        .event(TestCounter::CountDecreased::SIGNATURE.to_owned());
    let mut decr_stream = scanner.subscribe(decrease_filter);

    scanner.start().await?;

    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    contract.decrease().send().await?.watch().await?;
    contract.decrease().send().await?.watch().await?;

    assert_event_sequence_final!(
        incr_stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(1) },
            TestCounter::CountIncreased { newCount: U256::from(2) }
        ]
    );
    assert_event_sequence_final!(
        decr_stream,
        &[
            TestCounter::CountDecreased { newCount: U256::from(1) },
            TestCounter::CountDecreased { newCount: U256::from(0) }
        ]
    );

    Ok(())
}

#[tokio::test]
async fn signature_matching_ignores_irrelevant_events() -> anyhow::Result<()> {
    let setup = setup_live_scanner(Some(0.1), None, 0).await?;
    let contract = setup.contract;
    let mut scanner = setup.scanner;

    // Subscribe to CountDecreased but only emit CountIncreased
    let filter = EventFilter::new()
        .contract_address(*contract.address())
        .event(TestCounter::CountDecreased::SIGNATURE.to_owned());

    let stream = scanner.subscribe(filter);

    scanner.start().await?;

    contract.increase().send().await?.watch().await?;

    assert_empty!(stream);

    Ok(())
}

#[tokio::test]
async fn filters_malformed_signature_graceful() -> anyhow::Result<()> {
    let setup = setup_live_scanner(Some(0.1), None, 0).await?;
    let contract = setup.contract;
    let mut scanner = setup.scanner;

    let filter =
        EventFilter::new().contract_address(*contract.address()).event("invalid-sig".to_string());

    let stream = scanner.subscribe(filter);

    scanner.start().await?;

    contract.increase().send().await?.watch().await?;

    assert_empty!(stream);

    Ok(())
}
