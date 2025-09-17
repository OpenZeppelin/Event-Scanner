use std::sync::Arc;

use alloy::primitives::Address;

use crate::callback::EventCallback;

#[derive(Clone)]
pub struct EventFilter {
    pub contract_address: Address,
    /// Human-readable event signature, e.g. "Transfer(address,address,uint256)".
    /// TODO: Maybe change this to selector i.e. B256
    pub event: String,
    pub callback: Arc<dyn EventCallback + Send + Sync>,
}
