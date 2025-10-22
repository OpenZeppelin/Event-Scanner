use crate::event_scanner::{filter::EventFilter, scanner::EventScannerMessage};
use tokio::sync::mpsc::Sender;

#[derive(Clone)]
pub struct EventListener {
    pub filter: EventFilter,
    pub sender: Sender<EventScannerMessage>,
}
