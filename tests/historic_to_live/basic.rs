use alloy::{eips::BlockNumberOrTag, network::Ethereum, primitives::U256, sol_types::SolEvent};
use event_scanner::{event_filter::EventFilter, event_scanner::EventScanner, types::ScannerStatus};

use crate::{
    assert_next_logs, assert_next_status,
    common::{TestCounter, build_provider, deploy_counter, spawn_anvil},
};

#[tokio::test]
async fn replays_historical_then_switches_to_live() -> anyhow::Result<()> {
    let anvil = spawn_anvil(Some(0.1))?;
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

    let mut client = EventScanner::new().connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;

    let mut stream = client.create_event_stream(filter);

    tokio::spawn(async move {
        client.start_scanner(BlockNumberOrTag::Number(first_historical_block), None).await
    });

    for _ in 0..live_events {
        contract.increase().send().await?.watch().await?;
    }

    // historical events
    assert_next_logs!(
        stream,
        &[
            TestCounter::CountIncreased { newCount: U256::from(1) },
            TestCounter::CountIncreased { newCount: U256::from(2) },
            TestCounter::CountIncreased { newCount: U256::from(3) },
        ]
    );

    // chain tip reached
    assert_next_status!(stream, ScannerStatus::ChainTipReached);

    // live events
    assert_next_logs!(stream, &[TestCounter::CountIncreased { newCount: U256::from(4) },]);
    assert_next_logs!(stream, &[TestCounter::CountIncreased { newCount: U256::from(5) },]);

    Ok(())
}
