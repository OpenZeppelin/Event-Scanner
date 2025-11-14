use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::common::{TestCounter, deploy_counter, setup_live_scanner};
use alloy::{primitives::U256, sol_types::SolEvent};
use event_scanner::{EventFilter, Message, assert_empty, assert_next};
use tokio::time::timeout;
use tokio_stream::StreamExt;

#[tokio::test]
async fn basic_single_event_scanning() -> anyhow::Result<()> {
    let setup = setup_live_scanner(Some(0.1), None, 0).await?;
    let contract = setup.contract.clone();
    let expected_event_count = 5;

    let scanner = setup.scanner;
    let mut stream = setup.stream.take(expected_event_count);

    scanner.start().await?;

    for _ in 0..expected_event_count {
        contract.increase().send().await?.watch().await?;
    }

    let event_count = Arc::new(AtomicUsize::new(0));
    let event_count_clone = Arc::clone(&event_count);
    let event_counting = async move {
        let mut expected_new_count = 1;
        while let Some(message) = stream.next().await {
            match message {
                Message::Data(logs) => {
                    event_count_clone.fetch_add(logs.len(), Ordering::SeqCst);

                    for log in logs {
                        let TestCounter::CountIncreased { newCount } =
                            log.log_decode().unwrap().inner.data;
                        assert_eq!(newCount, expected_new_count);
                        expected_new_count += 1;
                    }
                }
                Message::Error(e) => {
                    panic!("panicked with error: {e}");
                }
                Message::Status(_) => {
                    // Handle info if needed
                }
            }
        }
    };

    _ = timeout(Duration::from_secs(1), event_counting).await;

    assert_eq!(event_count.load(Ordering::SeqCst), expected_event_count);

    Ok(())
}

#[tokio::test]
async fn multiple_contracts_same_event_isolate_callbacks() -> anyhow::Result<()> {
    let setup = setup_live_scanner(Some(0.1), None, 0).await?;
    let provider = setup.provider.clone();
    let a = setup.contract.clone();
    let b = deploy_counter(provider.primary().clone()).await?;

    let a_filter = EventFilter::new()
        .contract_address(*a.address())
        .event(TestCounter::CountIncreased::SIGNATURE.to_owned());
    let b_filter = EventFilter::new()
        .contract_address(*b.address())
        .event(TestCounter::CountIncreased::SIGNATURE.to_owned());

    let mut scanner = setup.scanner;

    let mut a_stream = scanner.subscribe(a_filter);
    let mut b_stream = scanner.subscribe(b_filter);

    scanner.start().await?;

    a.increase().send().await?.watch().await?;
    a.increase().send().await?.watch().await?;
    a.increase().send().await?.watch().await?;

    b.increase().send().await?.watch().await?;
    b.increase().send().await?.watch().await?;

    assert_next!(a_stream, &[TestCounter::CountIncreased { newCount: U256::from(1) }]);
    assert_next!(a_stream, &[TestCounter::CountIncreased { newCount: U256::from(2) }]);
    assert_next!(a_stream, &[TestCounter::CountIncreased { newCount: U256::from(3) }]);
    assert_empty!(a_stream);

    assert_next!(b_stream, &[TestCounter::CountIncreased { newCount: U256::from(1) }]);
    assert_next!(b_stream, &[TestCounter::CountIncreased { newCount: U256::from(2) }]);
    assert_empty!(b_stream);

    Ok(())
}

#[tokio::test]
async fn multiple_events_same_contract() -> anyhow::Result<()> {
    let setup = setup_live_scanner(Some(0.1), None, 0).await?;
    let contract = setup.contract.clone();
    let contract_address = *contract.address();

    let increase_filter = EventFilter::new()
        .contract_address(contract_address)
        .event(TestCounter::CountIncreased::SIGNATURE.to_owned());
    let decrease_filter = EventFilter::new()
        .contract_address(contract_address)
        .event(TestCounter::CountDecreased::SIGNATURE.to_owned());

    let mut scanner = setup.scanner;

    let mut incr_stream = scanner.subscribe(increase_filter);
    let mut decr_stream = scanner.subscribe(decrease_filter);

    scanner.start().await?;

    contract.increase().send().await?.watch().await?;
    contract.increase().send().await?.watch().await?;

    contract.decrease().send().await?.watch().await?;
    contract.decrease().send().await?.watch().await?;

    assert_next!(incr_stream, &[TestCounter::CountIncreased { newCount: U256::from(1) }]);
    assert_next!(incr_stream, &[TestCounter::CountIncreased { newCount: U256::from(2) }]);
    assert_empty!(incr_stream);

    assert_next!(decr_stream, &[TestCounter::CountDecreased { newCount: U256::from(1) }]);
    assert_next!(decr_stream, &[TestCounter::CountDecreased { newCount: U256::from(0) }]);
    assert_empty!(decr_stream);

    Ok(())
}

#[tokio::test]
async fn signature_matching_ignores_irrelevant_events() -> anyhow::Result<()> {
    let setup = setup_live_scanner(Some(0.1), None, 0).await?;
    let contract = setup.contract.clone();

    // Subscribe to CountDecreased but only emit CountIncreased
    let filter = EventFilter::new()
        .contract_address(*contract.address())
        .event(TestCounter::CountDecreased::SIGNATURE.to_owned());

    let num_of_events = 3;

    let mut scanner = setup.scanner;

    let mut stream = scanner.subscribe(filter).take(num_of_events);

    scanner.start().await?;

    for _ in 0..num_of_events {
        contract.increase().send().await?.watch().await?;
    }

    let event_counting = async move {
        _ = stream.next().await;
    };

    if timeout(Duration::from_secs(1), event_counting).await.is_ok() {
        anyhow::bail!("scanner should have ignored all of the emitted events");
    }

    Ok(())
}

#[tokio::test]
async fn live_filters_malformed_signature_graceful() -> anyhow::Result<()> {
    let setup = setup_live_scanner(Some(0.1), None, 0).await?;
    let contract = setup.contract.clone();

    let filter =
        EventFilter::new().contract_address(*contract.address()).event("invalid-sig".to_string());

    let num_of_events = 3;

    let mut scanner = setup.scanner;

    let mut stream = scanner.subscribe(filter).take(num_of_events);

    scanner.start().await?;

    for _ in 0..num_of_events {
        contract.increase().send().await?.watch().await?;
    }

    let event_counting = async move {
        _ = stream.next().await;
    };

    if timeout(Duration::from_secs(1), event_counting).await.is_ok() {
        anyhow::bail!("scanner should have ignored all of the emitted events");
    }

    Ok(())
}
