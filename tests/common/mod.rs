#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(missing_docs)]

pub mod setup_scanner;
pub mod test_counter;

pub(crate) use setup_scanner::{
    setup_common, setup_historic_scanner, setup_latest_scanner, setup_live_scanner,
    setup_sync_from_latest_scanner, setup_sync_scanner,
};
pub(crate) use test_counter::{TestCounter, TestCounterExt, deploy_counter};

use std::sync::Arc;

use alloy::{
    eips::BlockNumberOrTag,
    network::Ethereum,
    primitives::FixedBytes,
    providers::{Provider, ProviderBuilder, ext::AnvilApi},
    rpc::types::anvil::{ReorgOptions, TransactionData},
};
use alloy_node_bindings::{Anvil, AnvilInstance};
use event_scanner::robust_provider::RobustProvider;

pub async fn reorg_with_new_count_incr_txs<P>(
    anvil: &AnvilInstance,
    contract: TestCounter::TestCounterInstance<Arc<P>>,
    num_initial_events: u64,
    num_new_events: u64,
    reorg_depth: u64,
    same_block: bool,
) -> anyhow::Result<Vec<FixedBytes<32>>>
where
    P: Provider<Ethereum> + Clone,
{
    let wallet = anvil.wallet().expect("anvil should return a default wallet");
    let provider = ProviderBuilder::new().wallet(wallet).connect(anvil.endpoint().as_str()).await?;
    let mut event_tx_hashes = vec![];

    for _ in 0..num_initial_events {
        let receipt = contract.increase().send().await.unwrap().get_receipt().await.unwrap();
        event_tx_hashes.push(receipt.transaction_hash);
    }

    let mut tx_block_pairs = vec![];
    for i in 0..num_new_events {
        let tx = contract.increase().into_transaction_request();
        tx_block_pairs.push((TransactionData::JSON(tx), if same_block { 0 } else { i }));
    }

    let pre_reorg_block = provider.get_block_by_number(BlockNumberOrTag::Latest).await?.unwrap();

    provider.anvil_reorg(ReorgOptions { depth: reorg_depth, tx_block_pairs }).await.unwrap();

    let post_reorg_block = provider
        .get_block_by_number(BlockNumberOrTag::Number(pre_reorg_block.number()))
        .full()
        .await?
        .unwrap();

    assert_eq!(post_reorg_block.header.number, pre_reorg_block.header.number);
    assert_ne!(post_reorg_block.header.hash, pre_reorg_block.header.hash);

    if same_block {
        let new_block = provider
            .get_block_by_number(BlockNumberOrTag::Number(
                post_reorg_block.header.number - reorg_depth + 1,
            ))
            .await?
            .unwrap();
        assert_eq!(new_block.transactions.len() as u64, num_new_events);
        for tx_hash in new_block.transactions.hashes() {
            event_tx_hashes.push(tx_hash);
        }
    } else {
        for i in 0..num_new_events {
            let new_block = provider
                .get_block_by_number(BlockNumberOrTag::Number(
                    post_reorg_block.header.number - reorg_depth + 1 + i,
                ))
                .await?
                .unwrap();
            assert_eq!(new_block.transactions.len() as u64, 1);
            for tx_hash in new_block.transactions.hashes() {
                event_tx_hashes.push(tx_hash);
            }
        }
    }

    Ok(event_tx_hashes)
}

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
    let robust_provider = RobustProvider::new(provider.root().to_owned());
    Ok(robust_provider)
}
