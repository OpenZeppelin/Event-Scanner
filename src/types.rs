use std::{error::Error, fmt::Debug};

use alloy::primitives::BlockHash;
use tokio::sync::mpsc;
use tracing::{info, warn};

#[derive(Copy, Debug, Clone)]
pub enum ScannerMessage<T: Clone, E: Error + Clone> {
    Data(T),
    Error(E),
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

    /// Emitted during the latest events phase when logs are found, containing the block
    /// hash of the first (oldest) log that will be delivered.
    FirstLogBlock(BlockHash),
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
