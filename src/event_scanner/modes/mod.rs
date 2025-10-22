mod historic;
mod latest;
mod live;
mod sync;

pub use historic::{HistoricEventScanner, HistoricScannerBuilder};
pub use latest::{LatestEventScanner, LatestScannerBuilder};
pub use live::{LiveEventScanner, LiveScannerBuilder};
pub use sync::{SyncEventScanner, SyncScannerBuilder};

use crate::block_range_scanner::BlockRangeScanner;

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

#[derive(Clone)]
pub struct BaseConfig {
    block_range_scanner: BlockRangeScanner,
}

impl BaseConfig {
    fn new() -> Self {
        Self { block_range_scanner: BlockRangeScanner::new() }
    }
}

pub trait BaseConfigBuilder: Sized {
    fn base_mut(&mut self) -> &mut BaseConfig;

    #[must_use]
    fn max_block_range(mut self, max_block_range: u64) -> Self {
        self.base_mut().block_range_scanner.max_block_range = max_block_range;
        self
    }
}
