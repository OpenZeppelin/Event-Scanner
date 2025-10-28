use std::{ops::RangeInclusive, sync::Arc};

use alloy::{
    eips::BlockNumberOrTag,
    primitives::BlockNumber,
    transports::{RpcError, TransportErrorKind},
};
use thiserror::Error;

use crate::{block_range_scanner::Message, robust_provider::Error as RobustProviderError};

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

    #[error("Only one subscriber allowed at a time")]
    MultipleSubscribers,

    #[error("No subscriber set for streaming")]
    NoSubscriber,

    #[error("Historical sync failed: {0}")]
    HistoricalSyncError(String),

    #[error("Block not found, block number: {0}")]
    BlockNotFound(BlockNumberOrTag),

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

impl From<Result<RangeInclusive<BlockNumber>, ScannerError>> for Message {
    fn from(logs: Result<RangeInclusive<BlockNumber>, ScannerError>) -> Self {
        match logs {
            Ok(logs) => Message::Data(logs),
            Err(e) => Message::Error(e),
        }
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

impl From<ScannerError> for Message {
    fn from(error: ScannerError) -> Self {
        Message::Error(error)
    }
}
