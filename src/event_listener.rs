use crate::{event_filter::EventFilter, event_scanner::EventScannerMessage};
use tokio::sync::mpsc::Sender;

#[derive(Clone)]
pub struct EventListener {
    pub filter: EventFilter,
    pub sender: Sender<EventScannerMessage>,
}
