use std::sync::Arc;

use alloy::{
    eips::BlockNumberOrTag,
    transports::{RpcError, TransportErrorKind, http::reqwest},
};
use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub enum ScannerError {
    #[error("HTTP request failed: {0}")]
    HttpError(Arc<reqwest::Error>),

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

    #[error("WebSocket connection failed after {0} attempts")]
    WebSocketConnectionFailed(usize),

    #[error("Block not found, block number: {0}")]
    BlockNotFound(BlockNumberOrTag),
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
