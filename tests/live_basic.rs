use std::{sync::{Arc, atomic::{AtomicUsize, Ordering}}, time::Duration};

mod common;
use alloy::{network::Ethereum, providers::WsConnect, sol_types::SolEvent};
use common::{TestCounter, EventCounter, build_provider, deploy_counter, spawn_anvil};
use event_scanner::{types::EventFilter, event_scanner::EventScannerBuilder};
use tokio::time::sleep;

#[tokio::test]
async fn live_basic_single_event_scanning() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();

    let event_count = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(EventCounter { count: event_count.clone() });

    let filter = EventFilter {
        contract_address,
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback,
    };

    let builder = EventScannerBuilder::<Ethereum>::new().with_event_filter(filter);
    let mut scanner = builder.connect_ws(WsConnect::new(anvil.ws_endpoint_url())).await?;

    let scanner_handle = tokio::spawn(async move { scanner.start().await });

    for _ in 0..5 {
        let _ = contract.increase().send().await?.get_receipt().await?;
    }

    sleep(Duration::from_millis(200)).await;
    scanner_handle.abort();

    assert_eq!(event_count.load(Ordering::SeqCst), 5);
    Ok(())
}

