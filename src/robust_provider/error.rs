use std::sync::Arc;

use alloy::{
    eips::BlockId,
    transports::{RpcError, TransportErrorKind},
};
use thiserror::Error;
use tokio::time::error as TokioError;

#[derive(Error, Debug, Clone)]
pub enum Error {
    #[error("Operation timed out")]
    Timeout,
    #[error("RPC call failed after exhausting all retry attempts: {0}")]
    RpcError(Arc<RpcError<TransportErrorKind>>),
    #[error("Block not found, Block Id: {0}")]
    BlockNotFound(BlockId),
}

impl From<RpcError<TransportErrorKind>> for Error {
    fn from(err: RpcError<TransportErrorKind>) -> Self {
        Error::RpcError(Arc::new(err))
    }
}

impl From<TokioError::Elapsed> for Error {
    fn from(_: TokioError::Elapsed) -> Self {
        Error::Timeout
    }
}
