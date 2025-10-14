use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use alloy::{network::Ethereum, sol_types::SolEvent};
use event_scanner::{EventFilter, EventScanner, EventScannerMessage};
use tokio::time::timeout;
use tokio_stream::StreamExt;

use crate::common::{TestCounter, build_provider, deploy_counter, spawn_anvil};

#[tokio::test]
async fn high_event_volume_no_loss() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.05)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;

    let filter = EventFilter::new()
        .with_contract_address(*contract.address())
        .with_event(TestCounter::CountIncreased::SIGNATURE);
    let expected_event_count = 100;

    let mut scanner = EventScanner::live().connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;

    let mut stream = scanner.create_event_stream(filter).take(expected_event_count);

    tokio::spawn(async move { scanner.start().await });

    for _ in 0..expected_event_count {
        contract.increase().send().await?.watch().await?;
    }

    let event_count = Arc::new(AtomicUsize::new(0));
    let event_count_clone = Arc::clone(&event_count);
    let event_counting = async move {
        let mut expected_new_count = 1;
        while let Some(EventScannerMessage::Data(logs)) = stream.next().await {
            event_count_clone.fetch_add(logs.len(), Ordering::SeqCst);

            for log in logs {
                let TestCounter::CountIncreased { newCount } = log.log_decode().unwrap().inner.data;
                assert_eq!(newCount, expected_new_count);
                expected_new_count += 1;
            }
        }
    };

    _ = timeout(Duration::from_secs(60), event_counting).await;

    assert_eq!(event_count.load(Ordering::SeqCst), expected_event_count);

    Ok(())
}
