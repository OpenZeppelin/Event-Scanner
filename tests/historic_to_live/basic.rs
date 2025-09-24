use std::sync::Arc;

use alloy::{eips::BlockNumberOrTag, network::Ethereum, sol_types::SolEvent};
use event_scanner::{event_filters::EventFilter, event_scanner::EventScannerBuilder};
use tokio::time::{Duration, sleep, timeout};

use crate::{
    common::{TestCounter, build_provider, deploy_counter, spawn_anvil},
    mock_callbacks::EventOrderingCallback,
};

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

    let event_new_counts = Arc::new(tokio::sync::Mutex::new(Vec::<u64>::new()));
    let callback = Arc::new(EventOrderingCallback { counts: Arc::clone(&event_new_counts) });

    let filter = EventFilter {
        contract_address: Some(contract_address),
        event: Some(TestCounter::CountIncreased::SIGNATURE.to_owned()),
        callback,
    };

    let mut scanner = EventScannerBuilder::new()
        .with_event_filter(filter)
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;

    tokio::spawn(async move {
        scanner.start(BlockNumberOrTag::Number(first_historical_block), None).await
    });

    sleep(Duration::from_millis(200)).await;

    for _ in 0..live_events {
        contract.increase().send().await?.watch().await?;
    }

    let event_new_counts_clone = Arc::clone(&event_new_counts);
    let event_counting = async move {
        while event_new_counts_clone.lock().await.len() < historical_events + live_events {
            sleep(Duration::from_millis(100)).await;
        }
    };

    if timeout(Duration::from_secs(1), event_counting).await.is_err() {
        assert_eq!(event_new_counts.lock().await.len(), historical_events + live_events);
    }

    let event_new_counts = event_new_counts.lock().await;

    let mut expected_new_count = 1;
    for &new_count in event_new_counts.iter() {
        assert_eq!(new_count, expected_new_count);
        expected_new_count += 1;
    }

    Ok(())
}
