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
use tokio::time::{sleep, timeout};

#[tokio::test]
async fn callbacks_slow_processing_does_not_drop_events() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();

    let processed = Arc::new(AtomicUsize::new(0));
    let callback =
        Arc::new(SlowProcessorCallback { delay_ms: 100, processed: Arc::clone(&processed) });

    let filter = EventFilter {
        contract_address,
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback,
    };
    let mut scanner = EventScannerBuilder::new()
        .with_event_filter(filter)
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;

    tokio::spawn(async move { scanner.start(BlockNumberOrTag::Latest, None).await });

    let expected_event_count = 3;

    for _ in 0..expected_event_count {
        // emits faster than processing to simulate backlog
        contract.increase().send().await?.watch().await?;
    }

    let event_counting = async move {
        while processed.load(Ordering::SeqCst) < expected_event_count {
            sleep(Duration::from_millis(100)).await;
        }
    };

    if timeout(Duration::from_secs(1), event_counting).await.is_err() {
        anyhow::bail!("scanner did not finish within 1 second");
    };

    Ok(())
}

#[tokio::test]
async fn callbacks_failure_then_retry_success() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;

    let attempts = Arc::new(AtomicUsize::new(0));
    let successes = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(FlakyCallback {
        attempts: attempts.clone(),
        successes: successes.clone(),
        max_fail_times: 2,
    });

    let filter = EventFilter {
        contract_address: *contract.address(),
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback,
    };
    let cfg = FixedRetryConfig { max_attempts: 3, delay_ms: 50 };

    let strategy: Arc<dyn CallbackStrategy> = Arc::new(FixedRetryStrategy::new(cfg));

    let mut scanner = EventScannerBuilder::new()
        .with_event_filter(filter)
        .with_callback_strategy(strategy)
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;

    tokio::spawn(async move { scanner.start(BlockNumberOrTag::Latest, None).await });

    contract.increase().send().await?.watch().await?;

    let attempt_counting = async move {
        while attempts.load(Ordering::SeqCst) < 3 || successes.load(Ordering::SeqCst) < 1 {
            sleep(Duration::from_millis(100)).await;
        }
    };

    if timeout(Duration::from_secs(1), attempt_counting).await.is_err() {
        anyhow::bail!("scanner did not finish within 1 second");
    };

    Ok(())
}

#[tokio::test]
async fn callbacks_always_failing_respects_max_attempts() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;

    let attempts = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(AlwaysFailingCallback { attempts: Arc::clone(&attempts) });

    let filter = EventFilter {
        contract_address: *contract.address(),
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback,
    };
    let cfg = FixedRetryConfig { max_attempts: 2, delay_ms: 20 };

    let strategy: Arc<dyn CallbackStrategy> = Arc::new(FixedRetryStrategy::new(cfg));

    let mut scanner = EventScannerBuilder::new()
        .with_event_filter(filter)
        .with_callback_strategy(strategy)
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;

    tokio::spawn(async move { scanner.start(BlockNumberOrTag::Latest, None).await });

    contract.increase().send().await?.watch().await?;

    let attempt_counting = async move {
        while attempts.load(Ordering::SeqCst) < 2 {
            sleep(Duration::from_millis(100)).await;
        }
    };

    if timeout(Duration::from_secs(1), attempt_counting).await.is_err() {
        anyhow::bail!("scanner did not finish within 1 second");
    };

    Ok(())
}
