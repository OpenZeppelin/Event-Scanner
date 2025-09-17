use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::{
    common::{TestCounter, build_provider, deploy_counter, spawn_anvil},
    mock_callbacks::{AlwaysFailingCallback, FlakyCallback, SlowProcessorCallback},
};
use alloy::{eips::BlockNumberOrTag, network::Ethereum, sol_types::SolEvent};
use event_scanner::{
    CallbackStrategy, EventFilter,
    callback_strategy::{FixedRetryConfig, FixedRetryStrategy},
    event_scanner::EventScannerBuilder,
};
use tokio::time::sleep;

#[tokio::test]
async fn callbacks_slow_processing_does_not_drop_events() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1.0)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();

    let processed = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(SlowProcessorCallback { delay_ms: 100, processed: processed.clone() });

    let filter = EventFilter {
        contract_address,
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback,
    };
    let mut builder = EventScannerBuilder::new();
    builder.with_event_filter(filter);
    let scanner = builder.connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;

    let scanner_handle = tokio::spawn(async move {
        let mut scanner = scanner;
        scanner.start(BlockNumberOrTag::Latest, None).await
    });

    for _ in 0..3 {
        let _ = contract.increase().send().await?.get_receipt().await?;
        // emit faster than processing to simulate backlog
        sleep(Duration::from_millis(50)).await;
    }

    sleep(Duration::from_millis(200)).await;
    scanner_handle.abort();

    assert_eq!(processed.load(Ordering::SeqCst), 3);
    Ok(())
}

#[tokio::test]
async fn callbacks_failure_then_retry_success() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1.0)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;

    let attempts = Arc::new(AtomicUsize::new(0));
    let successes = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(FlakyCallback {
        attempts: attempts.clone(),
        successes: successes.clone(),
        fail_times: 2,
    });

    let filter = EventFilter {
        contract_address: *contract.address(),
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback,
    };
    let cfg = FixedRetryConfig { max_attempts: 3, delay_ms: 50 };

    let mut builder = EventScannerBuilder::new();
    builder.with_event_filter(filter);
    let strategy: Arc<dyn CallbackStrategy> = Arc::new(FixedRetryStrategy::new(cfg));
    builder.with_callback_strategy(strategy);
    let scanner = builder.connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;
    let scanner_handle = tokio::spawn(async move {
        let mut scanner = scanner;
        scanner.start(BlockNumberOrTag::Latest, None).await
    });

    let _ = contract.increase().send().await?.get_receipt().await?;
    sleep(Duration::from_millis(300)).await;
    scanner_handle.abort();

    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert_eq!(successes.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn callbacks_always_failing_respects_max_attempts() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1.0)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;

    let attempts = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(AlwaysFailingCallback { attempts: attempts.clone() });

    let filter = EventFilter {
        contract_address: *contract.address(),
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback,
    };
    let cfg = FixedRetryConfig { max_attempts: 2, delay_ms: 20 };
    let mut builder = EventScannerBuilder::new();
    builder.with_event_filter(filter);
    let strategy: Arc<dyn CallbackStrategy> = Arc::new(FixedRetryStrategy::new(cfg));
    builder.with_callback_strategy(strategy);
    let scanner = builder.connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;
    let scanner_handle = tokio::spawn(async move {
        let mut scanner = scanner;
        scanner.start(BlockNumberOrTag::Latest, None).await
    });

    let _ = contract.increase().send().await?.get_receipt().await?;
    sleep(Duration::from_millis(200)).await;
    scanner_handle.abort();

    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    Ok(())
}
