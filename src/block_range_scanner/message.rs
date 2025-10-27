use std::ops::RangeInclusive;

use alloy::{
    primitives::BlockNumber,
    transports::{RpcError, TransportErrorKind},
};
use tokio::sync::mpsc;
use tracing::warn;

use crate::{ScannerError, ScannerMessage};

pub type Message = ScannerMessage<RangeInclusive<BlockNumber>, ScannerError>;

impl From<RangeInclusive<BlockNumber>> for Message {
    fn from(logs: RangeInclusive<BlockNumber>) -> Self {
        Message::Data(logs)
    }
}

impl PartialEq<RangeInclusive<BlockNumber>> for Message {
    fn eq(&self, other: &RangeInclusive<BlockNumber>) -> bool {
        if let Message::Data(range) = self { range.eq(other) } else { false }
    }
}

impl From<RpcError<TransportErrorKind>> for Message {
    fn from(error: RpcError<TransportErrorKind>) -> Self {
        Message::Error(error.into())
    }
}

impl From<ScannerError> for Message {
    fn from(error: ScannerError) -> Self {
        Message::Error(error)
    }
}

pub(super) trait TryStream {
    async fn try_stream<T: Into<Message>>(&self, msg: T) -> bool;
}

impl TryStream for mpsc::Sender<Message> {
    async fn try_stream<T: Into<Message>>(&self, msg: T) -> bool {
        if let Err(err) = self.send(msg.into()).await {
            warn!(error = %err, "Downstream channel closed, stopping stream");
            return false;
        }
        true
    }
}
