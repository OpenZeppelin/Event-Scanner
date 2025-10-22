mod historic;
mod latest;
mod live;
mod sync;

pub use historic::{HistoricEventScanner, HistoricScannerBuilder};
pub use latest::{LatestEventScanner, LatestScannerBuilder};
pub use live::{LiveEventScanner, LiveScannerBuilder};
pub use sync::{SyncEventScanner, SyncScannerBuilder};

pub struct EventScanner;

impl EventScanner {
    #[must_use]
    pub fn historic() -> HistoricScannerBuilder {
        HistoricScannerBuilder::new()
    }

    #[must_use]
    pub fn live() -> LiveScannerBuilder {
        LiveScannerBuilder::new()
    }

    #[must_use]
    pub fn sync() -> SyncScannerBuilder {
        SyncScannerBuilder::new()
    }

    #[must_use]
    pub fn latest() -> LatestScannerBuilder {
        LatestScannerBuilder::new()
    }
}
