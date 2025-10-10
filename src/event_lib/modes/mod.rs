mod historic;
mod latest;
mod live;
mod sync;

pub use historic::{HistoricModeConfig, HistoricModeScanner};
// pub use latest::{ConnectedLatestMode, LatestMode};
pub use live::{LiveModeConfig, LiveModeScanner};
pub use sync::{SyncModeConfig, SyncModeScanner};

use crate::{block_range_scanner::BlockRangeScanner, event_lib::EventFilter};

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
    event_filters: Vec<EventFilter>,
    block_range_scanner: BlockRangeScanner,
}

impl BaseConfig {
    fn new() -> Self {
        Self { event_filters: Vec::new(), block_range_scanner: BlockRangeScanner::new() }
    }
}

pub trait BaseConfigBuilder: Sized {
    fn base_mut(&mut self) -> &mut BaseConfig;

    #[must_use]
    fn event_filter(mut self, filter: EventFilter) -> Self {
        self.base_mut().event_filters.push(filter);
        self
    }

    #[must_use]
    fn event_filters(mut self, filters: Vec<EventFilter>) -> Self {
        self.base_mut().event_filters.extend(filters);
        self
    }

    #[must_use]
    fn max_reads(mut self, max: usize) -> Self {
        self.base_mut().block_range_scanner.max_read_per_epoch = max;
        self
    }
}

pub type Result<T> = std::result::Result<T, Error>;
pub type Error = Box<dyn std::error::Error + Send + Sync>;

pub struct EventStream;
