use crate::block_range_scanner;
use alloy::{primitives::Address, rpc::types::Log};
use tokio::sync::mpsc::Sender;

#[derive(Clone)]
pub struct EventFilter {
    pub contract_address: Address,
    /// Human-readable event signature, e.g. "Transfer(address,address,uint256)".
    pub event: String,
    pub sender: Sender<Result<Vec<Log>, block_range_scanner::Error>>,
}
