use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use alloy::{eips::BlockNumberOrTag, network::Ethereum, sol_types::SolEvent};
use event_scanner::{event_filter::EventFilter, event_scanner::EventScanner};
use tokio::time::{Duration, sleep, timeout};
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

    let filter = EventFilter {
        contract_address: Some(contract_address),
        event: Some(TestCounter::CountIncreased::SIGNATURE.to_owned()),
    };

    let mut client = EventScanner::new().connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;

    let mut stream = client.subscribe(filter).take(historical_events + live_events);

    tokio::spawn(async move {
        client.start_scanner(BlockNumberOrTag::Number(first_historical_block), None).await
    });

    sleep(Duration::from_millis(200)).await;

    for _ in 0..live_events {
        contract.increase().send().await?.watch().await?;
    }

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

    _ = timeout(Duration::from_secs(1), event_counting).await;

    assert_eq!(event_count.load(Ordering::SeqCst), historical_events + live_events);

    Ok(())
}
