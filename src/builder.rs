use crate::{FixedRetryConfig, scanner::Scanner, types::EventFilter};

pub struct ScannerBuilder {
    rpc_url: String,
    start_block: Option<u64>,
    end_block: Option<u64>,
    max_blocks_per_filter: u64,
    tracked_events: Vec<EventFilter>,
    callback_config: FixedRetryConfig,
}

impl ScannerBuilder {
    pub fn new<S: Into<String>>(rpc_url: S) -> Self {
        Self {
            rpc_url: rpc_url.into(),
            start_block: None,
            end_block: None,
            max_blocks_per_filter: 1000,
            tracked_events: Vec::new(),
            callback_config: FixedRetryConfig::default(),
        }
    }

    #[must_use]
    pub fn start_block(mut self, start_block: u64) -> Self {
        self.start_block = Some(start_block);
        self
    }

    #[must_use]
    pub fn end_block(mut self, end_block: u64) -> Self {
        self.end_block = Some(end_block);
        self
    }

    #[must_use]
    pub fn max_blocks_per_filter(mut self, max_blocks: u64) -> Self {
        self.max_blocks_per_filter = max_blocks;
        self
    }

    #[must_use]
    pub fn add_event_filter(mut self, filter: EventFilter) -> Self {
        self.tracked_events.push(filter);
        self
    }

    #[must_use]
    pub fn add_event_filters(mut self, filters: Vec<EventFilter>) -> Self {
        self.tracked_events.extend(filters);
        self
    }

    #[must_use]
    pub fn callback_config(mut self, cfg: FixedRetryConfig) -> Self {
        self.callback_config = cfg;
        self
    }

    /// Builds the scanner
    ///
    /// # Errors
    ///
    /// Returns an error if the scanner fails to build
    pub async fn build(self) -> anyhow::Result<Scanner> {
        Scanner::new(
            self.rpc_url,
            self.start_block,
            self.end_block,
            self.max_blocks_per_filter,
            self.tracked_events,
            self.callback_config,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        FixedRetryConfig, callback::EventCallback, callback_strategy::BACK_OFF_MAX_RETRIES,
    };
    use alloy::{primitives::address, rpc::types::Log};
    use async_trait::async_trait;
    use std::sync::Arc;

    struct MockCallback;

    #[async_trait]
    impl EventCallback for MockCallback {
        async fn on_event(&self, _log: &Log) -> anyhow::Result<()> {
            Ok(())
        }
    }

    const WS_URL: &str = "ws://localhost:8545";

    #[test]
    fn test_builder_new_defaults() {
        let builder = ScannerBuilder::new(WS_URL);
        assert_eq!(builder.rpc_url, WS_URL);
        assert_eq!(builder.start_block, None);
        assert_eq!(builder.end_block, None);
        assert_eq!(builder.max_blocks_per_filter, 1000);
        assert!(builder.tracked_events.is_empty());
    }

    #[test]
    fn test_builder_start_block() {
        let start_block = 100;
        let builder = ScannerBuilder::new(WS_URL).start_block(start_block);
        assert_eq!(builder.start_block, Some(start_block));
    }

    #[test]
    fn test_builder_end_block() {
        let end_block = 500;
        let builder = ScannerBuilder::new(WS_URL).end_block(end_block);
        assert_eq!(builder.end_block, Some(end_block));
    }

    #[test]
    fn test_builder_block_range() {
        let start_block = 100;
        let end_block = 500;
        let builder = ScannerBuilder::new(WS_URL).start_block(start_block).end_block(end_block);
        assert_eq!(builder.start_block, Some(start_block));
        assert_eq!(builder.end_block, Some(end_block));
    }

    #[test]
    fn test_builder_max_blocks_per_filter() {
        let max_blocks = 5000;
        let builder = ScannerBuilder::new(WS_URL).max_blocks_per_filter(max_blocks);
        assert_eq!(builder.max_blocks_per_filter, max_blocks);
    }

    #[test]
    fn test_builder_callback_config() {
        let max_attempts = 5;
        let delay_ms = 500;
        let config = FixedRetryConfig { max_attempts, delay_ms };

        let builder = ScannerBuilder::new(WS_URL).callback_config(config);

        assert_eq!(builder.callback_config.max_attempts, max_attempts);
        assert_eq!(builder.callback_config.delay_ms, delay_ms);
    }

    #[test]
    fn test_builder_default_callback_config() {
        let builder = ScannerBuilder::new(WS_URL);

        assert_eq!(builder.callback_config.max_attempts, BACK_OFF_MAX_RETRIES);
        assert_eq!(builder.callback_config.delay_ms, 200);
    }

    #[test]
    fn test_builder_add_event_filter() {
        let addr = address!("f39Fd6e51aad88F6F4ce6aB8827279cffFb92266");
        let event = "Transfer(address,address,uint256)".to_string();
        let filter = EventFilter {
            contract_address: addr,
            event: event.clone(),
            callback: Arc::new(MockCallback),
        };
        let builder = ScannerBuilder::new(WS_URL).add_event_filter(filter.clone());

        assert_eq!(builder.tracked_events.len(), 1);
        assert_eq!(builder.tracked_events[0].contract_address, addr);
        assert_eq!(builder.tracked_events[0].event, event);
    }

    #[test]
    fn test_builder_add_multiple_event_filters() {
        let addr1 = address!("f39Fd6e51aad88F6F4ce6aB8827279cffFb92266");
        let event1 = "Transfer(address,address,uint256)".to_string();
        let addr2 = address!("70997970C51812dc3A010C7d01b50e0d17dc79C8");
        let event2 = "Approval(address,address,uint256)".to_string();

        let filter1 = EventFilter {
            contract_address: addr1,
            event: event1.clone(),
            callback: Arc::new(MockCallback),
        };
        let filter2 = EventFilter {
            contract_address: addr2,
            event: event2.clone(),
            callback: Arc::new(MockCallback),
        };

        let builder = ScannerBuilder::new(WS_URL)
            .add_event_filter(filter1.clone())
            .add_event_filter(filter2.clone());

        assert_eq!(builder.tracked_events.len(), 2);
        for (i, expected_filter) in builder.tracked_events.iter().enumerate() {
            assert_eq!(
                builder.tracked_events[i].contract_address,
                expected_filter.contract_address
            );
            assert_eq!(builder.tracked_events[i].event, expected_filter.event);
        }
    }

    #[test]
    fn test_builder_add_event_filters_batch() {
        let addr1 = address!("f39Fd6e51aad88F6F4ce6aB8827279cffFb92266");
        let event1 = "Transfer(address,address,uint256)".to_string();
        let addr2 = address!("70997970C51812dc3A010C7d01b50e0d17dc79C8");
        let event2 = "Approval(address,address,uint256)".to_string();
        let addr3 = address!("3C44CdDdB6a900fa2b585dd299e03d12FA4293BC");
        let event3 = "Mint(address,uint256)".to_string();

        let filter_1 = EventFilter {
            contract_address: addr1,
            event: event1.clone(),
            callback: Arc::new(MockCallback),
        };
        let filter_2 = EventFilter {
            contract_address: addr2,
            event: event2.clone(),
            callback: Arc::new(MockCallback),
        };
        let filter_3 = EventFilter {
            contract_address: addr3,
            event: event3.clone(),
            callback: Arc::new(MockCallback),
        };

        let filters = vec![filter_1.clone(), filter_2.clone(), filter_3.clone()];
        let builder = ScannerBuilder::new(WS_URL).add_event_filters(filters.clone());

        assert_eq!(builder.tracked_events.len(), filters.len());

        for (i, expected_filter) in filters.iter().enumerate() {
            assert_eq!(
                builder.tracked_events[i].contract_address,
                expected_filter.contract_address
            );
            assert_eq!(builder.tracked_events[i].event, expected_filter.event);
        }
    }

    #[test]
    fn test_builder_chain_all_methods() {
        let start_block = 100;
        let end_block = 500;

        let addr = address!("f39Fd6e51aad88F6F4ce6aB8827279cffFb92266");
        let event = "Transfer(address,address,uint256)".to_string();

        let filter = EventFilter {
            contract_address: addr,
            event: event.clone(),
            callback: Arc::new(MockCallback),
        };

        let max_attempts = 5;
        let delay_ms = 500;
        let config = FixedRetryConfig { max_attempts, delay_ms };

        let max_blocks_per_filter = 2000;
        let builder = ScannerBuilder::new(WS_URL)
            .start_block(start_block)
            .end_block(end_block)
            .max_blocks_per_filter(max_blocks_per_filter)
            .add_event_filter(filter.clone())
            .callback_config(config);

        assert_eq!(builder.start_block, Some(start_block));
        assert_eq!(builder.end_block, Some(end_block));
        assert_eq!(builder.max_blocks_per_filter, max_blocks_per_filter);
        assert_eq!(builder.tracked_events.len(), 1);
        assert_eq!(builder.callback_config.max_attempts, max_attempts);
        assert_eq!(builder.callback_config.delay_ms, delay_ms);
    }
}
