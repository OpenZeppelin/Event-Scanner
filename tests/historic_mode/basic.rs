use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use event_scanner::EventScannerMessage;
use tokio::time::timeout;
use tokio_stream::StreamExt;

use crate::common::{TestCounter, setup_historic_scanner};

#[tokio::test]
async fn processes_events_within_specified_historical_range() -> anyhow::Result<()> {
    let setup = setup_historic_scanner(
        Some(0.1),
        None,
        alloy::eips::BlockNumberOrTag::Earliest,
        alloy::eips::BlockNumberOrTag::Latest,
    )
    .await?;

    let contract = setup.contract.clone();
    let expected_event_count = 4;

    for _ in 0..expected_event_count {
        contract.increase().send().await?.watch().await?;
    }

    let scanner = setup.scanner;
    let mut stream = setup.stream.take(expected_event_count);

    tokio::spawn(async move { scanner.run().await });

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

    _ = timeout(Duration::from_secs(3), event_counting).await;

    assert_eq!(event_count.load(Ordering::SeqCst), expected_event_count);

    Ok(())
}
