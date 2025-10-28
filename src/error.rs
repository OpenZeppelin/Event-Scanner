use std::sync::Arc;

use alloy::{
    eips::BlockNumberOrTag,
    transports::{RpcError, TransportErrorKind, http::reqwest},
};
use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub enum ScannerError {
    // #[error("WebSocket error: {0}")]
    // WebSocketError(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("Serialization error: {0}")]
    SerializationError(Arc<serde_json::Error>),

    #[error("RPC error: {0}")]
    RpcError(Arc<RpcError<TransportErrorKind>>),

    #[error("Channel send error")]
    ChannelError,

    #[error("Service is shutting down")]
    ServiceShutdown,

    #[error("No subscriber set for streaming")]
    NoSubscriber,

    #[error("Historical sync failed: {0}")]
    HistoricalSyncError(String),

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

impl From<reqwest::Error> for ScannerError {
    fn from(error: reqwest::Error) -> Self {
        ScannerError::HttpError(Arc::new(error))
    }
}

impl From<serde_json::Error> for ScannerError {
    fn from(error: serde_json::Error) -> Self {
        ScannerError::SerializationError(Arc::new(error))
    }
}

impl From<RpcError<TransportErrorKind>> for ScannerError {
    fn from(error: RpcError<TransportErrorKind>) -> Self {
        ScannerError::RpcError(Arc::new(error))
    }
}
