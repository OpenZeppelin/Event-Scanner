//! Event-Scanner is a library made to stream EVM event logs.
//!
//! The main entry point is [`EventScanner`], built via [`EventScannerBuilder`] in one of the
//! supported modes (e.g. [`Historic`] or [`Live`]).
//!
//! After constructing a scanner, register one or more event subscriptions with
//! [`EventScanner::subscribe`], then call [`EventScanner::start`] to begin streaming.
//!
//! # Stream items
//!
//! Each subscription yields an [`EventScannerResult`]. Successful items are [`Message`] values,
//! which wrap either event batches or a [`Notification`] (see [`ScannerMessage`]).
//!
//! # Ordering
//!
//! Ordering is preserved *per subscription stream*. There is no global ordering guarantee across
//! different subscriptions.
//!
//! # Reorgs and finality
//!
//! When scanning non-finalized blocks, the scanner may detect chain reorganizations and will emit
//! [`Notification::ReorgDetected`]. Consumers should assume the same events might be delivered more
//! than once around reorgs (i.e. benign duplicates are possible).
//!
//! [`BlockNumberOrTag::Finalized`][finalized] is treated as the authoritative finality boundary
//! when the scanner needs one. In live mode, `block_confirmations` delays emission to reduce the
//! chance that already-emitted blocks are affected by shallow reorganizations.
//!
//! # Dedupe vs rollback
//!
//! Event-Scanner does not include a built-in deduplication utility. Depending on your
//! application, you can:
//!
//! - **Implement idempotency/deduplication** (for example, keyed by transaction hash and log index,
//!   optionally including block hash).
//! - **Handle reorgs by rollback**: interpret [`Notification::ReorgDetected`] as a signal to revert
//!   application state for blocks after the reported common ancestor.
//!
//! # Backpressure and lag
//!
//! Streams are buffered. If a consumer cannot keep up and an internal broadcast receiver lags,
//! the subscription stream yields [`ScannerError::Lagged`].
//!
//! # Robust providers
//!
//! The [`robust_provider`] module provides [`robust_provider::RobustProvider`], a wrapper that can
//! retry and fail over across multiple RPC endpoints.
//!
//! [finalized]: alloy::eips::BlockNumberOrTag::Finalized

#[macro_use]
mod logging;

pub mod block_range_scanner;
pub mod robust_provider;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;

mod error;
mod event_scanner;
mod types;

pub use block_range_scanner::{
    BlockRangeScanner, BlockRangeScannerBuilder, BlockScannerResult, DEFAULT_BLOCK_CONFIRMATIONS,
    DEFAULT_MAX_BLOCK_RANGE, DEFAULT_STREAM_BUFFER_CAPACITY, RingBufferCapacity,
    RingBufferCapacity as PastBlocksStorageCapacity,
};

pub use error::ScannerError;
pub use types::{Notification, ScannerMessage};

pub use event_scanner::{
    DEFAULT_MAX_CONCURRENT_FETCHES, EventFilter, EventScanner, EventScannerBuilder,
    EventScannerResult, Historic, LatestEvents, Live, Message, SyncFromBlock, SyncFromLatestEvents,
};
