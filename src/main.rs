use std::sync::Arc;

use alloy::{primitives::Address, rpc::types::Log};
use async_trait::async_trait;
use event_scanner::{CallbackConfig, EventCallback, EventFilter, ScannerBuilder};
use tracing_subscriber::EnvFilter;

struct ExampleCallback;

#[async_trait]
impl EventCallback for ExampleCallback {
    async fn on_event(&self, log: &Log) -> anyhow::Result<()> {
        println!("Received event log: {:?}", log);
        Ok(())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init();

    let anvil = alloy_node_bindings::Anvil::new()
        .block_time(1)
        .try_spawn()?;

    let mut scanner = ScannerBuilder::new(anvil.ws_endpoint_url())
        .add_event_filter(EventFilter {
            contract_address: Address::ZERO,
            event: "Transfer(address,address,uint256)".to_string(),
            callback: Arc::new(ExampleCallback),
        })
        .callback_config(CallbackConfig {
            max_attempts: 5,
            delay_ms: 300,
        })
        .build()
        .await?;

    scanner.start().await?;

    Ok(())
}
