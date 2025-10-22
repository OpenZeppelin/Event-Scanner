use crate::event_scanner::{filter::EventFilter, message::EventScannerMessage};
use tokio::sync::mpsc::Sender;

#[derive(Clone)]
pub(crate) struct EventListener {
    pub filter: EventFilter,
    pub sender: Sender<EventScannerMessage>,
}
