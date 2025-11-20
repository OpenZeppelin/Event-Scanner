use std::fmt::Debug;

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::ScannerError;

#[derive(Debug, Clone)]
pub enum ScannerMessage<T: Clone> {
    Data(T),
    Notification(Notification),
}

#[derive(Copy, Debug, Clone, PartialEq)]
pub enum Notification {
    SwitchingToLive,
    ReorgDetected,
}

impl<T: Clone> From<Notification> for ScannerMessage<T> {
    fn from(value: Notification) -> Self {
        ScannerMessage::Notification(value)
    }
}

impl<T: Clone> PartialEq<Notification> for ScannerMessage<T> {
    fn eq(&self, other: &Notification) -> bool {
        if let ScannerMessage::Notification(notification) = self {
            notification == other
        } else {
            false
        }
    }
}

pub trait IntoScannerMessageResult<T: Clone> {
    fn into_scanner_message_result(self) -> Result<ScannerMessage<T>, ScannerError>;
}

impl<T: Clone> IntoScannerMessageResult<T> for Result<ScannerMessage<T>, ScannerError> {
    fn into_scanner_message_result(self) -> Result<ScannerMessage<T>, ScannerError> {
        self
    }
}

impl<T: Clone> IntoScannerMessageResult<T> for ScannerMessage<T> {
    fn into_scanner_message_result(self) -> Result<ScannerMessage<T>, ScannerError> {
        Ok(self)
    }
}

impl<T: Clone, E: Into<ScannerError>> IntoScannerMessageResult<T> for E {
    fn into_scanner_message_result(self) -> Result<ScannerMessage<T>, ScannerError> {
        Err(self.into())
    }
}

impl<T: Clone> IntoScannerMessageResult<T> for Notification {
    fn into_scanner_message_result(self) -> Result<ScannerMessage<T>, ScannerError> {
        Ok(ScannerMessage::Notification(self))
    }
}

pub(crate) trait TryStream<T: Clone> {
    async fn try_stream<M: IntoScannerMessageResult<T>>(&self, msg: M) -> bool;
}

impl<T: Clone + Debug> TryStream<T> for mpsc::Sender<Result<ScannerMessage<T>, ScannerError>> {
    async fn try_stream<M: IntoScannerMessageResult<T>>(&self, msg: M) -> bool {
        let item = msg.into_scanner_message_result();
        match &item {
            Ok(msg) => info!(item = ?msg, "Sending message"),
            Err(err) => info!(error = ?err, "Sending error"),
        }
        if let Err(err) = self.send(item).await {
            warn!(error = %err, "Downstream channel closed, stopping stream");
            return false;
        }
        true
    }
}
