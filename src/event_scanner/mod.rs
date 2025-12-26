//! High-level event scanner API.
//!
//! This module re-exports the primary types used for scanning EVM logs:
//!
//! - [`EventScanner`] and [`EventScannerBuilder`] for constructing and running scanners.
//! - [`EventFilter`] for defining which contract addresses and event signatures to match.
//! - [`Message`] / [`EventScannerResult`] for consuming subscription streams.
//!
//! Mode marker types (e.g. [`Live`], [`Historic`]) are also re-exported.

pub mod block_range_handler;

mod builder;
mod filter;
mod listener;
mod message;
mod modes;
mod scanner;

pub use builder::{DEFAULT_MAX_CONCURRENT_FETCHES, EventScannerBuilder};
pub use filter::EventFilter;
pub use message::{EventScannerResult, Message};
pub use modes::{Historic, LatestEvents, Live, SyncFromBlock, SyncFromLatestEvents};
pub use scanner::{EventScanner, EventSubscription, StartProof};
