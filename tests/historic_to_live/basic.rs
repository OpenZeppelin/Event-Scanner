use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use alloy::{network::Ethereum, sol_types::SolEvent};
use event_scanner::{EventFilter, EventScanner, EventScannerMessage, ScannerStatus};
use tokio::{
    sync::Mutex,
    time::{Duration, timeout},
};
use tokio_stream::StreamExt;

use crate::common::{TestCounter, build_provider, deploy_counter, spawn_anvil};

#[tokio::test]
async fn replays_historical_then_switches_to_live() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;
    let contract_address = *contract.address();

    let historical_events = 3;
    let live_events = 2;

    let receipt = contract.increase().send().await?.get_receipt().await?;
    let first_historical_block =
        receipt.block_number.expect("historical receipt should contain block number");

    for _ in 1..historical_events {
        contract.increase().send().await?.watch().await?;
    }

    let filter = EventFilter::new()
        .with_contract_address(contract_address)
        .with_event(TestCounter::CountIncreased::SIGNATURE);

    let mut scanner = EventScanner::sync()
        .from_block(first_historical_block)
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;

    let mut stream = scanner.create_event_stream(filter).take(historical_events + live_events);

    tokio::spawn(async move { scanner.start().await });

    for _ in 0..live_events {
        contract.increase().send().await?.watch().await?;
    }

    let event_count = Arc::new(AtomicUsize::new(0));
    let event_count_clone = Arc::clone(&event_count);

    let chain_tip_reached = Arc::new(Mutex::new(false));
    let chain_tip_reached_clone = chain_tip_reached.clone();

    let event_counting = async move {
        let mut expected_new_count = 1;
        while let Some(message) = stream.next().await {
            match message {
                EventScannerMessage::Data(logs) => {
                    event_count_clone.fetch_add(logs.len(), Ordering::SeqCst);
                    for log in logs {
                        let TestCounter::CountIncreased { newCount } =
                            log.log_decode().unwrap().inner.data;
                        assert_eq!(newCount, expected_new_count);
                        expected_new_count += 1;
                    }
                }
                EventScannerMessage::Status(status) => {
                    if matches!(status, ScannerStatus::ChainTipReached) {
                        *chain_tip_reached_clone.lock().await = true;
                    }
                }
                EventScannerMessage::Error(e) => {
                    panic!("Error Reached {e}");
                }
            }
        }
    };

    _ = timeout(Duration::from_secs(1), event_counting).await;

    assert_eq!(event_count.load(Ordering::SeqCst), historical_events + live_events);
    assert!(*chain_tip_reached.lock().await);

    Ok(())
}
