//! High-level event scanner API.
//!
//! This module re-exports the primary types used for scanning EVM logs:
//!
//! - [`EventScanner`] and [`EventScannerBuilder`] for constructing and running scanners.
//! - [`EventFilter`] for defining which contract addresses and event signatures to match.
//! - [`Message`] / [`EventScannerResult`] for consuming subscription streams.
//!
//! Mode marker types (e.g. [`Live`], [`Historic`]) are also re-exported.

mod filter;
mod listener;
mod message;
mod scanner;

pub use filter::EventFilter;
pub use message::{EventScannerResult, Message};
pub use scanner::{
    DEFAULT_MAX_CONCURRENT_FETCHES, EventScanner, EventScannerBuilder, Historic, LatestEvents,
    Live, SyncFromBlock, SyncFromLatestEvents,
};
