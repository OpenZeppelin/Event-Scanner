use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use alloy::{eips::BlockNumberOrTag, network::Ethereum, sol_types::SolEvent};
use event_scanner::{
    event_filter::EventFilter,
    event_scanner::{EventScanner, EventScannerMessage},
};
use tokio::time::timeout;
use tokio_stream::StreamExt;

use crate::common::{TestCounter, build_provider, deploy_counter, spawn_anvil};

#[tokio::test]
async fn processes_events_within_specified_historical_range() -> anyhow::Result<()> {
    let anvil = spawn_anvil(Some(0.1))?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();

    let filter = EventFilter::new()
        .with_contract_address(contract_address)
        .with_event(TestCounter::CountIncreased::SIGNATURE);

    let receipt = contract.increase().send().await?.get_receipt().await?;
    let start_block = receipt.block_number.expect("receipt should contain block number");
    let mut end_block = 0;

    let expected_event_count = 4;

    for _ in 1..expected_event_count {
        let receipt = contract.increase().send().await?.get_receipt().await?;
        end_block = receipt.block_number.expect("receipt should contain block number");
    }

    let mut client = EventScanner::new().connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;
    let mut stream = client.create_event_stream(filter).take(expected_event_count);

    tokio::spawn(async move {
        client
            .start_scanner(
                BlockNumberOrTag::Number(start_block),
                Some(BlockNumberOrTag::Number(end_block)),
            )
            .await
    });

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
