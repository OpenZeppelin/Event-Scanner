use std::fmt::Debug;

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::ScannerError;

/// Messages streamed by the scanner to subscribers.
///
/// Each message represents either data or a notification about the scanner's state or behavior.
#[derive(Copy, Debug, Clone)]
pub enum ScannerMessage<T: Clone> {
    /// Data streamed to the subscriber.
    Data(T),

    /// Notification about scanner state changes or important events.
    Notification(Notification),
}

/// Notifications emitted by the scanner to signal state changes or important events.
#[derive(Copy, Debug, Clone, PartialEq)]
pub enum Notification {
    /// Emitted when transitioning from the latest events phase to live streaming mode
    /// in sync scanners.
    SwitchingToLive,

    /// Emitted when a blockchain reorganization is detected during scanning.
    ReorgDetected,

    /// Emitted during the latest events phase when no matching logs are found in the
    /// scanned range.
    NoPastLogsFound,
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

pub type ScannerResult<T> = Result<ScannerMessage<T>, ScannerError>;

pub trait IntoScannerResult<T: Clone> {
    fn into_scanner_message_result(self) -> ScannerResult<T>;
}

impl<T: Clone> IntoScannerResult<T> for ScannerResult<T> {
    fn into_scanner_message_result(self) -> ScannerResult<T> {
        self
    }
}

impl<T: Clone> IntoScannerResult<T> for ScannerMessage<T> {
    fn into_scanner_message_result(self) -> ScannerResult<T> {
        Ok(self)
    }
}

impl<T: Clone, E: Into<ScannerError>> IntoScannerResult<T> for E {
    fn into_scanner_message_result(self) -> ScannerResult<T> {
        Err(self.into())
    }
}

impl<T: Clone> IntoScannerResult<T> for Notification {
    fn into_scanner_message_result(self) -> ScannerResult<T> {
        Ok(ScannerMessage::Notification(self))
    }
}

pub(crate) trait TryStream<T: Clone> {
    async fn try_stream<M: IntoScannerResult<T>>(&self, msg: M) -> bool;
}

impl<T: Clone + Debug> TryStream<T> for mpsc::Sender<ScannerResult<T>> {
    async fn try_stream<M: IntoScannerResult<T>>(&self, msg: M) -> bool {
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
