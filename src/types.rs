use std::sync::Arc;

use alloy::primitives::Address;

use crate::callback::EventCallback;

#[derive(Clone)]
pub struct EventFilter {
    /// Contract address to filter events from. If None, events from all contracts will be tracked.
    pub contract_address: Option<Address>,
    /// Human-readable event signature, e.g. "Transfer(address,address,uint256)".
    /// If None, all events from the specified contract(s) will be tracked.
    pub event: Option<String>,
    pub callback: Arc<dyn EventCallback + Send + Sync>,
}
