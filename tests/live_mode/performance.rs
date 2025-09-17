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

    let count = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(BasicCounterCallback { count: count.clone() });
    let filter = EventFilter {
        contract_address: *contract.address(),
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback,
    };

    let mut builder = EventScannerBuilder::new();
    builder.with_event_filter(filter);
    let mut scanner = builder.connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;
    tokio::spawn(async move { scanner.start(BlockNumberOrTag::Latest, None).await });

    let expected_number_of_events = 100;

    for _ in 0..expected_number_of_events {
        contract.increase().send().await?.watch().await?;
    }

    let count = Arc::clone(&count);
    let counting = async move {
        while count.load(Ordering::SeqCst) < expected_number_of_events {
            sleep(Duration::from_millis(100)).await;
        }
    };

    if timeout(Duration::from_secs(60), counting).await.is_err() {
        anyhow::bail!("scanner did not finish within 60 seconds");
    };

    Ok(())
}
