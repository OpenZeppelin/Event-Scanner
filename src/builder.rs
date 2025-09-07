use crate::{
    scanner::Scanner,
    types::{CallbackConfig, EventFilter},
};

pub struct ScannerBuilder {
    rpc_url: String,
    start_block: Option<u64>,
    max_blocks_per_filter: u64,
    tracked_events: Vec<EventFilter>,
    callback_config: CallbackConfig,
}

impl ScannerBuilder {
    pub fn new<S: Into<String>>(rpc_url: S) -> Self {
        Self {
            rpc_url: rpc_url.into(),
            start_block: None,
            max_blocks_per_filter: 1000,
            tracked_events: Vec::new(),
            callback_config: CallbackConfig::default(),
        }
    }

    pub fn start_block(mut self, start_block: u64) -> Self {
        self.start_block = Some(start_block);
        self
    }

    pub fn max_blocks_per_filter(mut self, max_blocks: u64) -> Self {
        self.max_blocks_per_filter = max_blocks;
        self
    }

    pub fn add_event_filter(mut self, filter: EventFilter) -> Self {
        self.tracked_events.push(filter);
        self
    }

    pub fn add_event_filters(mut self, filters: Vec<EventFilter>) -> Self {
        self.tracked_events.extend(filters);
        self
    }

    pub fn callback_config(mut self, cfg: CallbackConfig) -> Self {
        self.callback_config = cfg;
        self
    }

    pub async fn build(self) -> anyhow::Result<Scanner> {
        Scanner::new(
            self.rpc_url,
            self.start_block,
            self.max_blocks_per_filter,
            self.tracked_events,
            self.callback_config,
        ).await
    }
}
