mod historic;
mod latest;
mod live;
mod sync;

pub use historic::{HistoricModeConfig, HistoricModeScanner};
// pub use latest::{ConnectedLatestMode, LatestMode};
pub use live::{LiveModeConfig, LiveModeScanner};
pub use sync::{SyncModeConfig, SyncModeScanner};

use crate::block_range_scanner::BlockRangeScanner;

pub struct EventScanner;

impl EventScanner {
    #[must_use]
    pub fn historic() -> HistoricModeConfig {
        HistoricModeConfig::new()
    }

    #[must_use]
    pub fn live() -> LiveModeConfig {
        LiveModeConfig::new()
    }

    #[must_use]
    pub fn sync() -> SyncModeConfig {
        SyncModeConfig::new()
    }

    // #[must_use]
    // pub fn latest() -> LatestMode {
    //     LatestMode::new()
    // }
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
    fn max_reads(mut self, max: usize) -> Self {
        self.base_mut().block_range_scanner.max_read_per_epoch = max;
        self
    }
}
