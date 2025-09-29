use alloy::rpc::types::anvil::{ReorgOptions, TransactionData};
use event_scanner::event_scanner::EventScannerError;
use std::{sync::Arc, time::Duration};
use tokio_stream::StreamExt;

use tokio::{sync::Mutex, time::timeout};

use crate::common::{TestCounter, build_provider, deploy_counter, spawn_anvil};
use alloy::{
    eips::BlockNumberOrTag, network::Ethereum, providers::ext::AnvilApi, sol_types::SolEvent,
};
use event_scanner::{event_filter::EventFilter, event_scanner::{EventScanner, EventScannerMessage}};

use event_scanner::block_range_scanner::BlockRangeScannerError;

#[tokio::test]
async fn reorg_error_propagation_to_event_stream() -> anyhow::Result<()> {
    let anvil = spawn_anvil(0.1).unwrap();
    let provider = build_provider(&anvil).await.unwrap();

    let contract = deploy_counter(provider.clone()).await?;
    let contract_address = *contract.address();

    let filter = EventFilter {
        contract_address: Some(contract_address),
        event: Some(TestCounter::CountIncreased::SIGNATURE.to_owned()),
    };

    let mut client = EventScanner::new().connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;

    let mut stream = client.create_event_stream(filter);

    tokio::spawn(async move { client.start_scanner(BlockNumberOrTag::Latest, None).await });

    contract.increase().send().await?.watch().await?;

    let mut tx_block_pairs = vec![];
    let tx = contract.increase().into_transaction_request();
    tx_block_pairs.push((TransactionData::JSON(tx), 0));

    let reorg_options = ReorgOptions { depth: 2, tx_block_pairs };
    provider.anvil_reorg(reorg_options).await.unwrap();

    let reorg_errors = Arc::new(Mutex::new(Vec::new()));
    let reorg_errors_clone = Arc::clone(&reorg_errors);

    let error_monitoring = async move {
        while let Some(message) = stream.next().await {
            match message {
                EventScannerMessage::Logs(_) => {}
                EventScannerMessage::Error(e) => {
                    if let EventScannerError::BlockRangeScanner(
                        BlockRangeScannerError::ReorgDetected,
                    ) = e
                    {
                        let mut errors = reorg_errors_clone.lock().await;
                        errors.push("ReorgDetected");
                    } else {
                        let mut errors = reorg_errors_clone.lock().await;
                        errors.push("Other");
                    }
                }
                EventScannerMessage::Info(_) => {}
            }
        }
    };

    let _ = timeout(Duration::from_secs(3), error_monitoring).await;

    let final_errors = reorg_errors.lock().await;

    assert_eq!(final_errors[0], "ReorgDetected");
    assert_eq!(final_errors.len(), 1);

    Ok(())
}
