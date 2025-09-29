use crate::{event_filter::EventFilter, event_scanner::EventScannerError};
use alloy::rpc::types::Log;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;

#[derive(Clone)]
pub struct EventListener {
    pub filter: EventFilter,
    pub sender: Sender<Result<Vec<Log>, Arc<EventScannerError>>>,
}
