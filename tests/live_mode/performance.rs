use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use alloy::{eips::BlockNumberOrTag, network::Ethereum, sol_types::SolEvent};
use event_scanner::{event_scanner::EventScannerBuilder, types::EventFilter};
use tokio::time::{sleep, timeout};

use crate::{
    common::{TestCounter, build_provider, deploy_counter, spawn_anvil},
    mock_callbacks::BasicCounterCallback,
};

#[tokio::test]
async fn high_event_volume_no_loss() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.05)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;

    let event_count = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(BasicCounterCallback { count: Arc::clone(&event_count) });
    let filter = EventFilter {
        contract_address: *contract.address(),
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback,
    };

    let mut scanner = EventScannerBuilder::new()
        .with_event_filter(filter)
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;
    tokio::spawn(async move { scanner.start(BlockNumberOrTag::Latest, None).await });

    let expected_event_count = 100;

    for _ in 0..expected_event_count {
        contract.increase().send().await?.watch().await?;
    }

    let event_count_clone = Arc::clone(&event_count);
    let event_counting = async move {
        while event_count_clone.load(Ordering::SeqCst) < expected_event_count {
            sleep(Duration::from_millis(100)).await;
        }
    };

    if timeout(Duration::from_secs(60), event_counting).await.is_err() {
        assert_eq!(event_count.load(Ordering::SeqCst), expected_event_count);
    };

    Ok(())
}
