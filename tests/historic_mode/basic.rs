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
async fn processes_events_within_specified_historical_range() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();

    let event_count = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(BasicCounterCallback { count: Arc::clone(&event_count) });

    let filter = EventFilter {
        contract_address,
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback,
    };

    let mut first_block = None;
    let mut last_block = 0;

    for _ in 0..4 {
        let receipt = contract.increase().send().await?.get_receipt().await?;
        let block_number = receipt.block_number.expect("receipt should contain block number");

        if first_block.is_none() {
            first_block = Some(block_number);
        }
        last_block = block_number;
    }

    let start_block = first_block.expect("at least one historical event");
    let end_block = last_block;

    let mut scanner = EventScannerBuilder::new()
        .with_event_filter(filter)
        .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
        .await?;

    tokio::spawn(async move {
        scanner
            .start(BlockNumberOrTag::Number(start_block), Some(BlockNumberOrTag::Number(end_block)))
            .await
    });

    let event_counting = async move {
        while event_count.load(Ordering::SeqCst) < 4 {
            sleep(Duration::from_millis(100)).await;
        }
    };

    if timeout(Duration::from_secs(3), event_counting).await.is_err() {
        anyhow::bail!("scanner did not finish within 3 seconds");
    };

    Ok(())
}
