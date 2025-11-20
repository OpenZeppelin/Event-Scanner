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

impl<T: Clone> From<ScannerMessage<T>> for Result<ScannerMessage<T>, ScannerError> {
    fn from(value: ScannerMessage<T>) -> Self {
        Ok(value)
    }
}

impl<T: Clone> From<ScannerError> for Result<ScannerMessage<T>, ScannerError> {
    fn from(value: ScannerError) -> Self {
        Err(value)
    }
}

impl<T: Clone> From<Notification> for Result<ScannerMessage<T>, ScannerError> {
    fn from(value: Notification) -> Self {
        Ok(value.into())
    }
}

pub(crate) trait TryStream<T: Clone> {
    async fn try_stream<M: Into<Result<ScannerMessage<T>, ScannerError>>>(&self, msg: M) -> bool;
}

impl<T: Clone + Debug> TryStream<T> for mpsc::Sender<Result<ScannerMessage<T>, ScannerError>> {
    async fn try_stream<M: Into<Result<ScannerMessage<T>, ScannerError>>>(&self, msg: M) -> bool {
        let item = msg.into();
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
