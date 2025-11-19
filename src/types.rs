use std::{error::Error, fmt::Debug};

use tokio::sync::mpsc;
use tracing::{info, warn};

#[derive(Copy, Debug, Clone)]
pub enum ScannerMessage<T: Clone, E: Error + Clone> {
    Data(T),
    Error(E),
    Notification(Notification),
}

#[derive(Copy, Debug, Clone, PartialEq)]
pub enum Notification {
    SwitchingToLive,
    ReorgDetected,
}

impl<T: Clone, E: Error + Clone> From<Notification> for ScannerMessage<T, E> {
    fn from(value: Notification) -> Self {
        ScannerMessage::Notification(value)
    }
}

impl<T: Clone, E: Error + Clone> PartialEq<Notification> for ScannerMessage<T, E> {
    fn eq(&self, other: &Notification) -> bool {
        if let ScannerMessage::Notification(notification) = self {
            notification == other
        } else {
            false
        }
    }
}

pub(crate) trait TryStream<T: Clone, E: Error + Clone> {
    async fn try_stream<M: Into<ScannerMessage<T, E>>>(&self, msg: M) -> bool;
}

impl<T: Clone + Debug, E: Error + Clone> TryStream<T, E> for mpsc::Sender<ScannerMessage<T, E>> {
    async fn try_stream<M: Into<ScannerMessage<T, E>>>(&self, msg: M) -> bool {
        let msg = msg.into();
        info!(msg = ?msg, "Sending message");
        if let Err(err) = self.send(msg).await {
            warn!(error = %err, "Downstream channel closed, stopping stream");
            return false;
        }
        true
    }
}
