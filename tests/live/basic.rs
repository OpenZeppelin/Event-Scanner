use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::common::{TestCounter, deploy_counter, setup_live_scanner};
use alloy::sol_types::SolEvent;
use event_scanner::{EventFilter, Message};
use tokio::time::timeout;
use tokio_stream::{StreamExt, wrappers::ReceiverStream};

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
    let b = deploy_counter(Arc::new(provider.clone())).await?;

    let a_filter = EventFilter::new()
        .contract_address(*a.address())
        .event(TestCounter::CountIncreased::SIGNATURE.to_owned());
    let b_filter = EventFilter::new()
        .contract_address(*b.address())
        .event(TestCounter::CountIncreased::SIGNATURE.to_owned());
    let expected_events_a = 3;
    let expected_events_b = 2;

    let mut scanner = setup.scanner;

    let a_stream = scanner.subscribe(a_filter);
    let b_stream = scanner.subscribe(b_filter);

    scanner.start().await?;

    for _ in 0..expected_events_a {
        a.increase().send().await?.watch().await?;
    }

    for _ in 0..expected_events_b {
        b.increase().send().await?.watch().await?;
    }

    let make_assertion = async |stream: ReceiverStream<Message>, expected_events| {
        let mut stream = stream.take(expected_events);

        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = Arc::clone(&count);

        let event_counting = async move {
            let mut expected_new_count = 1;
            while let Some(message) = stream.next().await {
                match message {
                    Message::Data(logs) => {
                        count_clone.fetch_add(logs.len(), Ordering::SeqCst);

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
        assert_eq!(count.load(Ordering::SeqCst), expected_events);
    };

    make_assertion(a_stream, expected_events_a).await;
    make_assertion(b_stream, expected_events_b).await;

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

    let expected_incr_events = 6;
    let expected_decr_events = 2;

    let mut scanner = setup.scanner;

    let mut incr_stream = scanner.subscribe(increase_filter).take(expected_incr_events);
    let mut decr_stream = scanner.subscribe(decrease_filter).take(expected_decr_events);

    scanner.start().await?;

    for _ in 0..expected_incr_events {
        contract.increase().send().await?.watch().await?;
    }

    contract.decrease().send().await?.watch().await?;
    contract.decrease().send().await?.watch().await?;

    let incr_count = Arc::new(AtomicUsize::new(0));
    let decr_count = Arc::new(AtomicUsize::new(0));
    let incr_count_clone = Arc::clone(&incr_count);
    let decr_count_clone = Arc::clone(&decr_count);

    let event_counting = async move {
        let mut expected_new_count = 0;

        // process CountIncreased
        while let Some(Message::Data(logs)) = incr_stream.next().await {
            incr_count_clone.fetch_add(logs.len(), Ordering::SeqCst);

            for log in logs {
                expected_new_count += 1;
                let TestCounter::CountIncreased { newCount } = log.log_decode().unwrap().inner.data;
                assert_eq!(newCount, expected_new_count);
            }
        }

        expected_new_count -= 1;

        // process CountDecreased
        while let Some(Message::Data(logs)) = decr_stream.next().await {
            decr_count_clone.fetch_add(logs.len(), Ordering::SeqCst);

            for log in logs {
                let TestCounter::CountDecreased { newCount } = log.log_decode().unwrap().inner.data;
                assert_eq!(newCount, expected_new_count);
                expected_new_count -= 1;
            }
        }
    };

    _ = timeout(Duration::from_secs(2), event_counting).await;

    assert_eq!(incr_count.load(Ordering::SeqCst), expected_incr_events);
    assert_eq!(decr_count.load(Ordering::SeqCst), expected_decr_events);

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
