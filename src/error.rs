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

    #[error("RPC call failed after exhausting all retry attempts: {0}")]
    RetryFailure(Arc<RpcError<TransportErrorKind>>),
}

impl From<RobustProviderError> for ScannerError {
    fn from(error: RobustProviderError) -> ScannerError {
        match error {
            RobustProviderError::Timeout => ScannerError::Timeout,
            RobustProviderError::RetryFailure(err) => ScannerError::RetryFailure(err),
            RobustProviderError::BlockNotFound(block) => ScannerError::BlockNotFound(block),
        }
    }
}

impl From<RpcError<TransportErrorKind>> for ScannerError {
    fn from(error: RpcError<TransportErrorKind>) -> Self {
        ScannerError::RpcError(Arc::new(error))
    }
}
