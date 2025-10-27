mod common;
mod historic;
mod latest;
mod live;
mod sync;

use alloy::network::Network;

pub use historic::{HistoricEventScanner, HistoricScannerBuilder};
pub use latest::{LatestEventScanner, LatestScannerBuilder};
pub use live::{LiveEventScanner, LiveScannerBuilder};
pub use sync::{SyncEventScanner, SyncScannerBuilder};

pub struct EventScanner;

impl EventScanner {
    #[must_use]
    pub fn historic<N: Network>() -> HistoricScannerBuilder<N> {
        HistoricScannerBuilder::new()
    }

    #[must_use]
    pub fn live<N: Network>() -> LiveScannerBuilder<N> {
        LiveScannerBuilder::new()
    }

    #[must_use]
    pub fn sync<N: Network>() -> SyncScannerBuilder<N> {
        SyncScannerBuilder::new()
    }

    #[must_use]
    pub fn latest<N: Network>() -> LatestScannerBuilder<N> {
        LatestScannerBuilder::new()
    }
}
