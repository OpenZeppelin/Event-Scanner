pub mod block_range_scanner;

pub mod robust_provider;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;

mod error;
mod event_scanner;
mod types;

pub use block_range_scanner::{
    DEFAULT_STREAM_BUFFER_CAPACITY, RingBufferCapacity as PastBlocksStorageCapacity,
};

pub use error::ScannerError;
pub use types::{Notification, ScannerMessage};

pub use event_scanner::{
    DEFAULT_MAX_CONCURRENT_FETCHES, EventFilter, EventScanner, EventScannerBuilder,
    EventScannerResult, Historic, LatestEvents, Live, Message, SyncFromBlock, SyncFromLatestEvents,
};
