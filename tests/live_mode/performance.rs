use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use alloy::{eips::BlockNumberOrTag, network::Ethereum, sol_types::SolEvent};
use event_scanner::{event_scanner::EventScanner, types::EventFilter};
use tokio::{sync::mpsc, time::timeout};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};

use crate::common::{TestCounter, build_provider, deploy_counter, spawn_anvil};

#[tokio::test]
async fn high_event_volume_no_loss() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.05)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;

    let (sender, receiver) = mpsc::channel(100);
    let filter = EventFilter {
        contract_address: *contract.address(),
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
        sender,
    };

    let mut scanner = EventScanner::new()
        .with_event_filter(filter)
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;
    tokio::spawn(async move { scanner.start(BlockNumberOrTag::Latest, None).await });

    let expected_event_count = 100;

    for _ in 0..expected_event_count {
        contract.increase().send().await?.watch().await?;
    }

    let mut stream = ReceiverStream::new(receiver).take(expected_event_count);

    let event_count = Arc::new(AtomicUsize::new(0));
    let event_count_clone = Arc::clone(&event_count);
    let event_counting = async move {
        let mut expected_new_count = 1;
        while let Some(Ok(logs)) = stream.next().await {
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
