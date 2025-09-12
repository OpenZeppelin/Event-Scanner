use std::time::Duration;

use crate::common;
use alloy::{network::Ethereum, providers::WsConnect, sol_types::SolEvent};
use common::{OrderingCaptureCount, TestCounter, build_provider, deploy_counter, spawn_anvil};
use event_scanner::{event_scanner::EventScannerBuilder, types::EventFilter};
use tokio::time::sleep;

#[tokio::test]
async fn events_processed_in_callback_order_by_count() -> anyhow::Result<()> {
    let anvil = spawn_anvil(1)?;
    let provider = build_provider(&anvil).await?;
    let contract = deploy_counter(provider).await?;

    let counts = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::<u64>::new()));
    let callback = std::sync::Arc::new(OrderingCaptureCount { counts: counts.clone() });

    let filter = EventFilter {
        contract_address: *contract.address(),
        event: TestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback,
    };
    let builder = EventScannerBuilder::<Ethereum>::new().with_event_filter(filter);
    let mut scanner = builder.connect_ws(WsConnect::new(anvil.ws_endpoint_url())).await?;
    let scanner_handle = tokio::spawn(async move { scanner.start().await });

    for _ in 0..5 {
        let _ = contract.increase().send().await?.get_receipt().await?;
    }

    sleep(Duration::from_millis(400)).await;
    scanner_handle.abort();

    let data = counts.lock().await;
    let expected: Vec<u64> = (1..=5).collect();
    assert_eq!(*data, expected, "callback ordering mismatch; counts: {:?}", *data);
    Ok(())
}
