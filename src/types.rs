use std::{error::Error, fmt::Debug};

use tokio::sync::mpsc;
use tracing::{info, warn};

#[derive(Copy, Debug, Clone)]
pub enum ScannerMessage<T: Clone, E: Error + Clone> {
    Data(T),
    Error(E),
    Notification(ScannerNotification),
}

#[derive(Copy, Debug, Clone, PartialEq)]
pub enum ScannerNotification {
    SwitchingToLive,
    ReorgDetected,
}

impl<T: Clone, E: Error + Clone> From<ScannerNotification> for ScannerMessage<T, E> {
    fn from(value: ScannerNotification) -> Self {
        ScannerMessage::Notification(value)
    }
}

impl<T: Clone, E: Error + Clone> PartialEq<ScannerNotification> for ScannerMessage<T, E> {
    fn eq(&self, other: &ScannerNotification) -> bool {
        if let ScannerMessage::Notification(status) = self { status == other } else { false }
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
