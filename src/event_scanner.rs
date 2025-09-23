use std::sync::Arc;

use crate::{
    block_range_scanner::{self, BlockRangeScanner, ConnectedBlockRangeScanner},
    callback::strategy::{CallbackStrategy, StateSyncAwareStrategy},
    types::EventFilter,
};
use alloy::{
    eips::BlockNumberOrTag, network::Network, providers::Provider, rpc::types::Filter,
    transports::http::reqwest::Url,
};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio_stream::StreamExt;
use tracing::{error, info};

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
        let client = self.block_range_scanner.run()?;
        let mut stream = client.subscribe(start_height, end_height).await?;

        // CHECK: How big should the broadcast channel be?
        // Also do we need a way to shutdown the broadcast channel?
        let (range_tx, _) = broadcast::channel::<(u64, u64)>(1024);

        for filter in &self.tracked_events {
            let provider = self.block_range_scanner.provider().clone();
            let mut sub = range_tx.subscribe();
            let event_name = filter.event.clone();
            let contract_address = filter.contract_address;
            let callback = filter.callback.clone();
            let strategy = self.callback_strategy.clone();

            tokio::spawn(async move {
                loop {
                    match sub.recv().await {
                        Ok((from_block, to_block)) => {
                            let filter = Filter::new()
                                .address(contract_address)
                                .event(event_name.as_str())
                                .from_block(from_block)
                                .to_block(to_block);

                            match provider.get_logs(&filter).await {
                                Ok(logs) => {
                                    if logs.is_empty() {
                                        continue;
                                    }
                                    info!(contract = ?contract_address, event = %event_name, log_count = logs.len(), from_block, to_block, "found logs for event in block range");
                                    for log in logs {
                                        if let Err(e) = strategy.execute(&callback, &log).await {
                                            error!(event = %event_name, at_block = &log.block_number, error = %e, "failed to invoke callback after retries");
                                        }
                                    }
                                }
                                Err(e) => {
                                    error!(contract = ?contract_address, event = %event_name, error = %e, from_block, to_block, "failed to get logs for block range");
                                }
                            }
                        }
                        // TODO: What happens if the broadcast channel is closed?
                        Err(RecvError::Closed) => break,
                        Err(RecvError::Lagged(_)) => {}
                    }
                }
            });
        }

        while let Some(range) = stream.next().await {
            match range {
                Ok(range) => {
                    let from_block = *range.start();
                    let to_block = *range.end();
                    info!(from_block, to_block, "processing block range");
                    if let Err(e) = range_tx.send((from_block, to_block)) {
                        error!(error = %e, "failed to send block range to broadcast channel");
                        break;
                    }
                }
                Err(e) => {
                    error!(error = %e, "failed to get block range");
                }
            }
        }

        Ok(())
    }
}
