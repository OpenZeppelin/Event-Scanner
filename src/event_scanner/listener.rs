use crate::event_scanner::{filter::EventFilter, message::Message};
use tokio::sync::mpsc::Sender;

#[derive(Clone)]
pub(crate) struct EventListener {
    pub filter: EventFilter,
    pub sender: Sender<Message>,
}
