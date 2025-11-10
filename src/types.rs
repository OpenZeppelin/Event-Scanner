use std::{error::Error, fmt::Debug};

use tokio::sync::mpsc;
use tracing::{info, warn};

#[derive(Copy, Debug, Clone)]
pub enum ScannerMessage<T: Clone, E: Error + Clone> {
    Data(T),
    Error(E),
    Status(ScannerStatus),
}

// TODO: implement Display for ScannerMessage

#[derive(Copy, Debug, Clone, PartialEq)]
pub enum ScannerStatus {
    SwitchingToLive,
    ReorgDetected,
}

impl<T: Clone, E: Error + Clone> From<ScannerStatus> for ScannerMessage<T, E> {
    fn from(value: ScannerStatus) -> Self {
        ScannerMessage::Status(value)
    }
}

impl<T: Clone, E: Error + Clone> PartialEq<ScannerStatus> for ScannerMessage<T, E> {
    fn eq(&self, other: &ScannerStatus) -> bool {
        if let ScannerMessage::Status(status) = self { status == other } else { false }
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
