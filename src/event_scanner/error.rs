use alloy::{
    rpc::types::Log,
    transports::{RpcError, TransportErrorKind},
};

use crate::{Message, ScannerError};

impl From<RpcError<TransportErrorKind>> for Message {
    fn from(e: RpcError<TransportErrorKind>) -> Self {
        Message::Error(e.into())
    }
}

impl From<ScannerError> for Message {
    fn from(error: ScannerError) -> Self {
        Message::Error(error)
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
