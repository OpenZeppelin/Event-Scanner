use std::{collections::HashMap, sync::Arc, time::Duration};

use crate::{
    block_scanner::{BlockScanner, BlockScannerError, ConnectedBlockScanner},
    callback::strategy::{CallbackStrategy, StateSyncAwareStrategy},
    types::EventFilter,
};
use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    providers::Provider,
    rpc::types::{Filter, Log},
    transports::http::reqwest::Url,
};
use tokio::sync::mpsc::{self, Receiver};
use tokio_stream::StreamExt;
use tracing::{error, info, warn};

pub struct EventScannerBuilder {
    block_scanner: BlockScanner,
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
    pub fn new() -> Self {
        Self {
            block_scanner: BlockScanner::new(),
            tracked_events: Vec::new(),
            callback_strategy: Self::get_default_callback_strategy(),
        }
    }

    pub fn with_event_filter(&mut self, filter: EventFilter) -> &mut Self {
        self.tracked_events.push(filter);
        self
    }

    pub fn with_event_filters(&mut self, filters: Vec<EventFilter>) -> &mut Self {
        self.tracked_events.extend(filters);
        self
    }

    pub fn with_callback_strategy(&mut self, strategy: Arc<dyn CallbackStrategy>) -> &mut Self {
        self.callback_strategy = strategy;
        self
    }

    pub fn with_blocks_read_per_epoch(&mut self, blocks_read_per_epoch: usize) -> &mut Self {
        let _ = self.block_scanner.with_blocks_read_per_epoch(blocks_read_per_epoch);
        self
    }

    pub fn with_reorg_rewind_depth(&mut self, reorg_rewind_depth: u64) -> &mut Self {
        let _ = self.block_scanner.with_reorg_rewind_depth(reorg_rewind_depth);
        self
    }

    pub fn with_retry_interval(&mut self, retry_interval: Duration) -> &mut Self {
        let _ = self.block_scanner.with_retry_interval(retry_interval);
        self
    }

    pub fn with_block_confirmations(&mut self, block_confirmations: u64) -> &mut Self {
        let _ = self.block_scanner.with_block_confirmations(block_confirmations);
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
    ) -> Result<EventScanner<N>, BlockScannerError> {
        let block_scanner = self.block_scanner.connect_ws(ws_url).await?;
        Ok(EventScanner {
            block_scanner,
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
    ) -> Result<EventScanner<N>, BlockScannerError> {
        let block_scanner = self.block_scanner.connect_ipc(ipc_path.into()).await?;
        Ok(EventScanner {
            block_scanner,
            tracked_events: self.tracked_events,
            callback_strategy: self.callback_strategy,
        })
    }

    fn get_default_callback_strategy() -> Arc<dyn CallbackStrategy> {
        let state_sync_aware_strategy = StateSyncAwareStrategy::new();
        Arc::new(state_sync_aware_strategy)
    }
}

pub struct EventScanner<N: Network> {
    block_scanner: ConnectedBlockScanner<N>,
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
        let mut event_channels: HashMap<String, mpsc::Sender<Log>> = HashMap::new();

        for filter in &self.tracked_events {
            let event_name = filter.event.clone();

            if event_channels.contains_key(&event_name) {
                continue;
            }

            // TODO: configurable buffer size / smaller buffer ?
            let (sender, receiver) = mpsc::channel::<Log>(1024);

            let event_name_clone = event_name.clone();
            let callback = filter.callback.clone();
            let strategy = self.callback_strategy.clone();
            Self::spawn_event_callback_task_executors(
                receiver,
                callback,
                strategy,
                event_name_clone,
            );

            event_channels.insert(event_name, sender);
        }

        let client = self.block_scanner.run()?;
        let mut stream = client.subscribe(start_height, end_height).await?;

        while let Some(range) = stream.next().await {
            match range {
                Ok(range) => {
                    let from_block = range.start;
                    let to_block = range.end;
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

    async fn process_block_range(
        &self,
        from_block: u64,
        to_block: u64,
        event_channels: &HashMap<String, mpsc::Sender<Log>>,
    ) -> anyhow::Result<()> {
        for event_filter in &self.tracked_events {
            let filter = Filter::new()
                .address(event_filter.contract_address)
                .event(event_filter.event.as_str())
                .from_block(from_block)
                .to_block(to_block);

            match self.block_scanner.provider().get_logs(&filter).await {
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

                    if let Some(sender) = event_channels.get(&event_filter.event) {
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
