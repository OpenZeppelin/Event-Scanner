use crate::event_scanner::{EventScannerResult, filter::EventFilter};
use tokio::sync::mpsc::Sender;

#[derive(Clone)]
pub(crate) struct EventListener {
    pub filter: EventFilter,
    pub sender: Sender<EventScannerResult>,
}
