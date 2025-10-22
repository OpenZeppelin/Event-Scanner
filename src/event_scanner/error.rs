use std::sync::Arc;

use alloy::{
    rpc::types::Log,
    transports::{RpcError, TransportErrorKind},
};
use thiserror::Error;

use crate::{
    block_range_scanner::BlockRangeScannerError, event_scanner::message::EventScannerMessage,
};

#[derive(Error, Debug, Clone)]
pub enum EventScannerError {
    #[error("Block range scanner error: {0}")]
    BlockRangeScanner(#[from] BlockRangeScannerError),
    #[error("Provider error: {0}")]
    Provider(Arc<RpcError<TransportErrorKind>>),
}

impl From<RpcError<TransportErrorKind>> for EventScannerError {
    fn from(e: RpcError<TransportErrorKind>) -> Self {
        EventScannerError::Provider(Arc::new(e))
    }
}

impl From<RpcError<TransportErrorKind>> for EventScannerMessage {
    fn from(e: RpcError<TransportErrorKind>) -> Self {
        EventScannerMessage::Error(e.into())
    }
}

impl From<BlockRangeScannerError> for EventScannerMessage {
    fn from(e: BlockRangeScannerError) -> Self {
        EventScannerMessage::Error(e.into())
    }
}

impl From<Result<Vec<Log>, RpcError<TransportErrorKind>>> for EventScannerMessage {
    fn from(logs: Result<Vec<Log>, RpcError<TransportErrorKind>>) -> Self {
        match logs {
            Ok(logs) => EventScannerMessage::Data(logs),
            Err(e) => EventScannerMessage::Error(e.into()),
        }
    }
}
