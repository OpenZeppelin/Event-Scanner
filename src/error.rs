use std::sync::Arc;

use alloy::{
    eips::BlockId,
    transports::{RpcError, TransportErrorKind},
};
use thiserror::Error;

use crate::robust_provider::Error as RobustProviderError;

#[derive(Error, Debug, Clone)]
pub enum ScannerError {
    #[error("RPC error: {0}")]
    RpcError(Arc<RpcError<TransportErrorKind>>),

    #[error("Service is shutting down")]
    ServiceShutdown,

    #[error("Block not found, Block Id: {0}")]
    BlockNotFound(BlockId),

    #[error("Operation timed out")]
    Timeout,

    #[error("{0} {1} exceeds the latest block {2}")]
    BlockExceedsLatest(&'static str, u64, u64),

    #[error("Event count must be greater than 0")]
    InvalidEventCount,

    #[error("Max block range must be greater than 0")]
    InvalidMaxBlockRange,
}

impl From<RobustProviderError> for ScannerError {
    fn from(error: RobustProviderError) -> ScannerError {
        match error {
            RobustProviderError::Timeout => ScannerError::Timeout,
            RobustProviderError::RpcError(err) => ScannerError::RpcError(err),
            RobustProviderError::BlockNotFound(block) => ScannerError::BlockNotFound(block),
        }
    }
}

impl From<RpcError<TransportErrorKind>> for ScannerError {
    fn from(error: RpcError<TransportErrorKind>) -> Self {
        ScannerError::RpcError(Arc::new(error))
    }
}
