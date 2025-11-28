pub mod block_range_scanner;

pub mod robust_provider;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;

mod error;
mod event_scanner;
mod types;

pub use block_range_scanner::RingBufferCapacity as PastBlocksStorageCapacity;

pub use error::ScannerError;
pub use types::{Notification, ScannerMessage};

pub use event_scanner::{
    EventFilter, EventScanner, EventScannerBuilder, EventScannerResult, Historic, LatestEvents,
    Live, Message, SyncFromBlock, SyncFromLatestEvents,
};
