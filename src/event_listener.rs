use crate::{block_range_scanner, event_filter::EventFilter};
use alloy::rpc::types::Log;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;

#[derive(Clone)]
pub struct EventListener {
    pub filter: EventFilter,
    pub sender: Sender<Result<Vec<Log>, Arc<block_range_scanner::Error>>>,
}
