use std::{error::Error, fmt::Debug};

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::ScannerError;

#[derive(Copy, Debug, Clone)]
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

pub(crate) trait TryStream<M> {
    async fn try_stream(&self, item: M) -> bool;
}

impl<T: Clone + Debug, M> TryStream<M> for mpsc::Sender<ScannerMessage<T>>
where
    M: Into<ScannerMessage<T>>,
{
    async fn try_stream(&self, item: M) -> bool {
        let item = item.into();
        info!(item = ?item, "Sending message");
        if let Err(err) = self.send(item).await {
            warn!(error = %err, "Downstream channel closed, stopping stream");
            return false;
        }
        true
    }
}

impl<E> TryStream<E> for mpsc::Sender<ScannerError>
where
    E: Error + Clone + Into<ScannerError>,
{
    async fn try_stream(&self, item: E) -> bool {
        let item = item.into();
        info!(item = ?item, "Sending error");
        if let Err(err) = self.send(item).await {
            warn!(error = %err, "Downstream channel closed, stopping stream");
            return false;
        }
        true
    }
}
