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

use crate::common::{TestCounter, setup_live_scanner};

#[tokio::test]
async fn high_event_volume_no_loss() -> anyhow::Result<()> {
    let setup = setup_live_scanner(Some(0.05), None, 0).await?;
    let contract = setup.contract.clone();

    let expected_event_count = 100;

    let scanner = setup.scanner;

    let mut stream = setup.stream.take(expected_event_count);

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
