use std::error::Error;

use tokio::sync::mpsc;
use tracing::warn;

#[derive(Copy, Debug, Clone)]
pub enum ScannerMessage<T: Clone, E: Error + Clone> {
    Data(T),
    Error(E),
    Status(ScannerStatus),
}

#[derive(Copy, Debug, Clone, PartialEq)]
pub enum ScannerStatus {
    ChainTipReached,
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

impl<T: Clone, E: Error + Clone> TryStream<T, E> for mpsc::Sender<ScannerMessage<T, E>> {
    async fn try_stream<M: Into<ScannerMessage<T, E>>>(&self, msg: M) -> bool {
        if let Err(err) = self.send(msg.into()).await {
            warn!(error = %err, "Downstream channel closed, stopping stream");
            return false;
        }
        true
    }
}
