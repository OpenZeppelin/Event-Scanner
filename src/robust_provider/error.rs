use std::sync::Arc;

use alloy::{
    eips::BlockId,
    transports::{RpcError, TransportErrorKind},
};
use thiserror::Error;
use tokio::{sync::broadcast::error::RecvError, time::error as TokioError};
use tracing::error;

#[derive(Error, Debug, Clone)]
pub enum Error {
    #[error("Operation timed out")]
    Timeout,
    #[error("RPC call failed after exhausting all retry attempts: {0}")]
    RpcError(Arc<RpcError<TransportErrorKind>>),
    #[error("Block not found, Block Id: {0}")]
    BlockNotFound(BlockId),
    #[error("Subscription closed")]
    Closed,
    #[error("Subscription lagged behind by: {0}")]
    Lagged(u64),
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

impl From<RecvError> for Error {
    fn from(err: RecvError) -> Self {
        match err {
            RecvError::Closed => {
                error!("Provider closed the subscription channel");
                Error::Closed
            }
            RecvError::Lagged(count) => {
                error!(skipped = count, "Receiver lagged");
                Error::Lagged(count)
            }
        }
    }
}
