mod historic;
mod latest;
mod live;
mod sync;

pub use historic::{HistoricEventScanner, HistoricScannerConfig};
pub use latest::{LatestEventScanner, LatestScannerConfig};
pub use live::{LiveEventScanner, LiveScannerConfig};
pub use sync::{SyncEventScanner, SyncScannerConfig};

use crate::block_range_scanner::BlockRangeScanner;

pub struct EventScanner;

impl EventScanner {
    #[must_use]
    pub fn historic() -> HistoricScannerConfig {
        HistoricScannerConfig::new()
    }

    #[must_use]
    pub fn live() -> LiveScannerConfig {
        LiveScannerConfig::new()
    }

    #[must_use]
    pub fn sync() -> SyncScannerConfig {
        SyncScannerConfig::new()
    }

    #[must_use]
    pub fn latest() -> LatestScannerConfig {
        LatestScannerConfig::new()
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
