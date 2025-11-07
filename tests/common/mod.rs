#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(missing_docs)]

pub mod setup_scanner;
pub mod test_counter;

pub(crate) use setup_scanner::{
    LiveScannerSetup, SyncScannerSetup, setup_common, setup_historic_scanner, setup_latest_scanner,
    setup_live_scanner, setup_sync_from_latest_scanner, setup_sync_scanner,
};
pub(crate) use test_counter::{TestCounter, deploy_counter};

use alloy::{network::Ethereum, providers::ProviderBuilder};
use alloy_node_bindings::{Anvil, AnvilInstance};
use event_scanner::robust_provider::RobustProvider;

pub fn spawn_anvil(block_time: Option<f64>) -> anyhow::Result<AnvilInstance> {
    let mut anvil = Anvil::new();
    if let Some(block_time) = block_time {
        anvil = anvil.block_time_f64(block_time);
    }
    Ok(anvil.try_spawn()?)
}

pub async fn build_provider(anvil: &AnvilInstance) -> anyhow::Result<RobustProvider<Ethereum>> {
    let wallet = anvil.wallet().expect("anvil should return a default wallet");
    let provider =
        ProviderBuilder::new().wallet(wallet).connect(anvil.ws_endpoint_url().as_str()).await?;
    let robust_provider = RobustProvider::new(provider).await?;
    Ok(robust_provider)
}
