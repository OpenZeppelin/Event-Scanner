use std::sync::Arc;

use alloy::{
    rpc::types::Log,
    transports::{RpcError, TransportErrorKind},
};
use thiserror::Error;

use crate::{block_range_scanner::BlockRangeScannerError, event_scanner::message::Message};

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

impl From<RpcError<TransportErrorKind>> for Message {
    fn from(e: RpcError<TransportErrorKind>) -> Self {
        Message::Error(e.into())
    }
}

impl From<BlockRangeScannerError> for Message {
    fn from(e: BlockRangeScannerError) -> Self {
        Message::Error(e.into())
    }
}

impl From<Result<Vec<Log>, RpcError<TransportErrorKind>>> for Message {
    fn from(logs: Result<Vec<Log>, RpcError<TransportErrorKind>>) -> Self {
        match logs {
            Ok(logs) => Message::Data(logs),
            Err(e) => Message::Error(e.into()),
        }
    }
}
