pub mod block_range_scanner;

pub mod robust_provider;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;

mod error;
mod event_scanner;
mod types;

pub use error::ScannerError;
pub use types::{ScannerMessage, ScannerStatus};

pub use event_scanner::{
    EventFilter, EventScanner, EventScannerBuilder, Historic, LatestEvents, Live, Message,
    SyncFromBlock, SyncFromLatestEvents,
};

pub use robust_provider::{
    provider::RobustProvider, provider_conversion::IntoRobustProvider,
    subscription::RobustSubscription,
};
