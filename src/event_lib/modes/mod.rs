mod historic;
mod latest;
mod subscribe;
mod sync;

use alloy::eips::BlockNumberOrTag;
pub use historic::{ConnectedHistoricMode, HistoricMode};
pub use latest::{ConnectedLatestMode, LatestMode};
pub use subscribe::{ConnectedSubscribeMode, SubscribeMode};
pub use sync::{ConnectedSyncMode, SyncMode};

use crate::{block_range_scanner::BlockRangeScanner, event_lib::EventFilter};

pub struct DummyEventScanner;

impl DummyEventScanner {
    #[must_use]
    pub fn historic() -> HistoricMode {
        HistoricMode::new()
    }

    #[must_use]
    pub fn subscribe() -> SubscribeMode {
        SubscribeMode::new()
    }

    #[must_use]
    pub fn sync() -> SyncMode {
        SyncMode::new()
    }

    #[must_use]
    pub fn latest() -> LatestMode {
        LatestMode::new()
    }
}

#[derive(Clone)]
struct BaseConfig {
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

    fn event_filter(mut self, filter: EventFilter) -> Self {
        self.base_mut().event_filters.push(filter);
        self
    }

    fn event_filters(mut self, filters: Vec<EventFilter>) -> Self {
        self.base_mut().event_filters.extend(filters);
        self
    }

    fn max_reads(mut self, max: usize) -> Self {
        self.base_mut().block_range_scanner.max_read_per_epoch = max;
        self
    }
}

pub type Result<T> = std::result::Result<T, Error>;
pub type Error = Box<dyn std::error::Error + Send + Sync>;

pub struct EventStream;
