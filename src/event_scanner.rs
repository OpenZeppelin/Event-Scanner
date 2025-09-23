use std::{collections::HashMap, sync::Arc};

use crate::{
    block_range_scanner::{self, BlockRangeScanner, ConnectedBlockRangeScanner},
    callback::strategy::{CallbackStrategy, StateSyncAwareStrategy},
    types::EventFilter,
};
use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    primitives::Address,
    providers::Provider,
    rpc::types::{Filter, Log},
    transports::http::reqwest::Url,
};
use tokio::sync::mpsc::{self, Receiver};
use tokio_stream::StreamExt;
use tracing::{error, info, warn};

pub struct EventScannerBuilder {
    block_range_scanner: BlockRangeScanner,
    tracked_events: Vec<EventFilter>,
    callback_strategy: Arc<dyn CallbackStrategy>,
}

impl Default for EventScannerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl EventScannerBuilder {
    #[must_use]
    /// Creates a new builder with default block scanner and callback strategy.
    pub fn new() -> Self {
        Self {
            block_range_scanner: BlockRangeScanner::new(),
            tracked_events: Vec::new(),
            callback_strategy: Self::get_default_callback_strategy(),
        }
    }

    /// Registers a single event filter for scanning.
    #[must_use]
    pub fn with_event_filter(mut self, filter: EventFilter) -> Self {
        self.tracked_events.push(filter);
        self
    }

    /// Registers a collection of event filters for scanning.
    #[must_use]
    pub fn with_event_filters(mut self, filters: Vec<EventFilter>) -> Self {
        self.tracked_events.extend(filters);
        self
    }

    /// Overrides the callback execution strategy used by the scanner.
    #[must_use]
    pub fn with_callback_strategy(mut self, strategy: Arc<dyn CallbackStrategy>) -> Self {
        self.callback_strategy = strategy;
        self
    }

    /// Configures how many blocks are read per epoch during a historical sync.
    #[must_use]
    pub fn with_blocks_read_per_epoch(mut self, blocks_read_per_epoch: usize) -> Self {
        self.block_range_scanner =
            self.block_range_scanner.with_blocks_read_per_epoch(blocks_read_per_epoch);
        self
    }

    /// Sets the depth to rewind when a reorg is detected.
    #[must_use]
    pub fn with_reorg_rewind_depth(mut self, reorg_rewind_depth: u64) -> Self {
        self.block_range_scanner =
            self.block_range_scanner.with_reorg_rewind_depth(reorg_rewind_depth);
        self
    }

    /// Configures how many confirmations are required before processing a block (used for reorgs).
    #[must_use]
    pub fn with_block_confirmations(mut self, block_confirmations: u64) -> Self {
        self.block_range_scanner =
            self.block_range_scanner.with_block_confirmations(block_confirmations);
        self
    }

    /// Connects to the provider via WebSocket
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ws<N: Network>(
        self,
        ws_url: Url,
    ) -> Result<EventScanner<N>, block_range_scanner::Error> {
        let block_range_scanner = self.block_range_scanner.connect_ws(ws_url).await?;
        Ok(EventScanner {
            block_range_scanner,
            tracked_events: self.tracked_events,
            callback_strategy: self.callback_strategy,
        })
    }

    /// Connects to the provider via IPC
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: impl Into<String>,
    ) -> Result<EventScanner<N>, block_range_scanner::Error> {
        let block_range_scanner = self.block_range_scanner.connect_ipc(ipc_path.into()).await?;
        Ok(EventScanner {
            block_range_scanner,
            tracked_events: self.tracked_events,
            callback_strategy: self.callback_strategy,
        })
    }

    /// Builds the default callback strategy used when none is provided.
    fn get_default_callback_strategy() -> Arc<dyn CallbackStrategy> {
        let state_sync_aware_strategy = StateSyncAwareStrategy::new();
        Arc::new(state_sync_aware_strategy)
    }
}

pub struct EventScanner<N: Network> {
    block_range_scanner: ConnectedBlockRangeScanner<N>,
    tracked_events: Vec<EventFilter>,
    callback_strategy: Arc<dyn CallbackStrategy>,
}

#[derive(Hash, Eq, PartialEq)]
struct EventIdentifier {
    contract_address: Address,
    event: String,
}

impl<N: Network> EventScanner<N> {
    /// Starts the scanner
    ///
    /// # Errors
    ///
    /// Returns an error if the scanner fails to start
    pub async fn start(
        &mut self,
        start_height: BlockNumberOrTag,
        end_height: Option<BlockNumberOrTag>,
    ) -> anyhow::Result<()> {
        let mut event_channels: HashMap<EventIdentifier, mpsc::Sender<Log>> = HashMap::new();

        for filter in &self.tracked_events {
            let unique_event = EventIdentifier {
                contract_address: filter.contract_address,
                event: filter.event.clone(),
            };

            if event_channels.contains_key(&unique_event) {
                continue;
            }

            let (sender, receiver) = mpsc::channel::<Log>(1024);

            let callback = filter.callback.clone();
            let strategy = self.callback_strategy.clone();
            Self::spawn_event_callback_task_executors(
                receiver,
                callback,
                strategy,
                filter.event.clone(),
            );

            event_channels.insert(unique_event, sender);
        }

        let client = self.block_range_scanner.run()?;
        let mut stream = client.subscribe(start_height, end_height).await?;

        while let Some(range) = stream.next().await {
            match range {
                Ok(range) => {
                    let from_block = *range.start();
                    let to_block = *range.end();
                    info!(from_block, to_block, "processing block range");
                    self.process_block_range(from_block, to_block, &event_channels).await?;
                }
                Err(e) => {
                    error!(error = %e, "failed to get block range");
                }
            }
        }

        Ok(())
    }

    /// Spawns background tasks that drive callback execution for an event type.
    fn spawn_event_callback_task_executors(
        mut receiver: Receiver<Log>,
        callback: Arc<dyn crate::callback::EventCallback + Send + Sync>,
        strategy: Arc<dyn CallbackStrategy>,
        event_name: String,
    ) {
        tokio::spawn(async move {
            while let Some(log) = receiver.recv().await {
                if let Err(e) = strategy.execute(&callback, &log).await {
                    error!(
                        event = %event_name,
                        at_block = &log.block_number,
                        error = %e,
                        "failed to invoke callback after retries"
                    );
                }
            }
        });
    }

    /// Fetches logs for the supplied inclusive block range [from_block..=to_block] and forwards them to the callback channels.
    async fn process_block_range(
        &self,
        from_block: u64,
        to_block: u64,
        event_channels: &HashMap<EventIdentifier, mpsc::Sender<Log>>,
    ) -> anyhow::Result<()> {
        for event_filter in &self.tracked_events {
            let filter = Filter::new()
                .address(event_filter.contract_address)
                .event(event_filter.event.as_str())
                .from_block(from_block)
                .to_block(to_block);

            match self.block_range_scanner.provider().get_logs(&filter).await {
                Ok(logs) => {
                    if logs.is_empty() {
                        continue;
                    }
                    info!(
                        contract = ?event_filter.contract_address,
                        event = %event_filter.event,
                        log_count = logs.len(),
                        from_block,
                        to_block,
                        "found logs for event in block range"
                    );

                    let event_identifier = EventIdentifier {
                        contract_address: event_filter.contract_address,
                        event: event_filter.event.clone(),
                    };

                    if let Some(sender) = event_channels.get(&event_identifier) {
                        for log in logs {
                            if let Err(e) = sender.send(log).await {
                                warn!(event = %event_filter.event, error = %e, "failed to enqueue log for processing");
                            }
                        }
                    } else {
                        warn!(event = %event_filter.event, "no channel found for event type");
                    }
                }
                Err(e) => {
                    error!(
                        contract = ?event_filter.contract_address,
                        event = %event_filter.event,
                        error = %e,
                        from_block,
                        to_block,
                        "failed to get logs for block range"
                    );
                }
            }
        }

        Ok(())
    }
}
