use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use alloy::{eips::BlockNumberOrTag, network::Ethereum, sol_types::SolEvent};
use event_scanner::{event_scanner::EventScannerBuilder, types::EventFilter};
use tokio::time::sleep;

use crate::{
    common::{TestCounter, build_provider, deploy_counter, spawn_anvil},
    mock_callbacks::BasicCounterCallback,
};

#[tokio::test]
async fn high_event_volume_no_loss() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1)?;
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
    let scanner = builder.connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;
    let scanner_handle = tokio::spawn(async move {
        let mut scanner = scanner;
        scanner.start(BlockNumberOrTag::Latest, None).await
    });

    for _ in 0..100 {
        let _ = contract.increase().send().await?.get_receipt().await?;
    }

    sleep(Duration::from_millis(1000)).await;
    scanner_handle.abort();

    assert_eq!(count.load(Ordering::SeqCst), 100);
    Ok(())
}
