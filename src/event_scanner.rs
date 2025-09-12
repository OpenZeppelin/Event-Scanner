#![allow(unused)]
use std::{cmp, collections::HashMap, future, sync::Arc, time::Duration};

use crate::{
    block_scanner::{
        BlockScanner, BlockScannerBuilder, OnBlocksFunc, STATE_SYNC_RETRY_INTERVAL,
        STATE_SYNC_RETRY_MAX_ELAPSED, STATE_SYNC_RETRY_MAX_INTERVAL, STATE_SYNC_RETRY_MULTIPLIER,
    },
    types::{CallbackConfig, EventFilter},
};
use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    providers::{IpcConnect, Provider, RootProvider, WsConnect},
    pubsub::PubSubConnect,
    rpc::{
        client::RpcClient,
        types::{Filter, Log},
    },
    transports::TransportError,
};
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tracing::{error, info, warn};

pub struct EventScannerBuilder<N: Network> {
    block_scanner: BlockScannerBuilder<N>,
    tracked_events: Vec<EventFilter>,
    callback_config: CallbackConfig,
}

impl<N: Network> Default for EventScannerBuilder<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N: Network> EventScannerBuilder<N> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            block_scanner: BlockScannerBuilder::new(),
            tracked_events: Vec::new(),
            callback_config: CallbackConfig::default(),
        }
    }

    #[must_use]
    pub fn with_event_filter(mut self, filter: EventFilter) -> Self {
        self.tracked_events.push(filter);
        self
    }

    #[must_use]
    pub fn with_event_filters(mut self, filters: Vec<EventFilter>) -> Self {
        self.tracked_events.extend(filters);
        self
    }

    #[must_use]
    pub fn with_callback_config(mut self, cfg: CallbackConfig) -> Self {
        self.callback_config = cfg;
        self
    }

    #[must_use]
    pub fn with_blocks_read_per_epoch(&mut self, blocks_read_per_epoch: usize) -> &mut Self {
        self.block_scanner.with_blocks_read_per_epoch(blocks_read_per_epoch);
        self
    }

    #[must_use]
    pub fn with_start_height(&mut self, start_height: BlockNumberOrTag) -> &mut Self {
        self.block_scanner.with_start_height(start_height);
        self
    }

    #[must_use]
    pub fn with_end_height(&mut self, end_height: BlockNumberOrTag) -> &mut Self {
        self.block_scanner.with_end_height(end_height);
        self
    }

    #[must_use]
    pub fn with_on_blocks(&mut self, on_blocks: OnBlocksFunc<N>) -> &mut Self {
        self.block_scanner.with_on_blocks(on_blocks);
        self
    }

    #[must_use]
    pub fn with_reorg_rewind_depth(&mut self, reorg_rewind_depth: u64) -> &mut Self {
        self.block_scanner.with_reorg_rewind_depth(reorg_rewind_depth);
        self
    }

    #[must_use]
    pub fn with_retry_interval(&mut self, retry_interval: Duration) -> &mut Self {
        self.block_scanner.with_retry_interval(retry_interval);
        self
    }

    #[must_use]
    pub fn with_block_confirmations(&mut self, block_confirmations: u64) -> &mut Self {
        self.block_scanner.with_block_confirmations(block_confirmations);
        self
    }

    /// Connects to the provider via WebSocket
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ws(
        self,
        connect: WsConnect,
    ) -> Result<EventScanner<RootProvider<N>, N>, TransportError> {
        let block_scanner = self.block_scanner.connect_ws(connect).await?;
        Ok(EventScanner {
            block_scanner,
            tracked_events: self.tracked_events,
            callback_config: self.callback_config,
        })
    }

    /// Connects to the provider via IPC
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ipc<T>(
        self,
        connect: IpcConnect<T>,
    ) -> Result<EventScanner<RootProvider<N>, N>, TransportError>
    where
        IpcConnect<T>: PubSubConnect,
    {
        let block_scanner = self.block_scanner.connect_ipc(connect).await?;
        Ok(EventScanner {
            block_scanner,
            tracked_events: self.tracked_events,
            callback_config: self.callback_config,
        })
    }

    #[must_use]
    pub fn connect_client(self, client: RpcClient) -> EventScanner<RootProvider<N>, N> {
        let block_scanner = self.block_scanner.connect_client(client);
        EventScanner {
            block_scanner,
            tracked_events: self.tracked_events,
            callback_config: self.callback_config,
        }
    }

    #[must_use]
    pub fn connect_provider(self, provider: RootProvider<N>) -> EventScanner<RootProvider<N>, N> {
        let block_scanner = self.block_scanner.connect_provider(provider);
        EventScanner {
            block_scanner,
            tracked_events: self.tracked_events,
            callback_config: self.callback_config,
        }
    }
}

pub struct EventScanner<P: Provider<N>, N: Network> {
    block_scanner: BlockScanner<P, N>,
    tracked_events: Vec<EventFilter>,
    callback_config: CallbackConfig,
}

impl<P: Provider<N>, N: Network> EventScanner<P, N> {
    /// Starts the scanner
    ///
    /// # Errors
    ///
    /// Returns an error if the scanner fails to start
    pub async fn start(&mut self) -> anyhow::Result<()> {
        let mut event_channels: HashMap<String, mpsc::Sender<Log>> = HashMap::new();

        for filter in &self.tracked_events {
            let event_name = filter.event.clone();
            if event_channels.contains_key(&event_name) {
                continue;
            }
            let (sender, mut receiver) = mpsc::channel::<Log>(1024); // TODO: configurable buffer size / smaller buffer ? 
            let cfg = self.callback_config.clone();
            let event_name_clone = event_name.clone();
            let callback = filter.callback.clone();
            tokio::spawn(async move {
                while let Some(log) = receiver.recv().await {
                    if let Err(e) = Self::smart_retry(&callback, &log, &cfg).await {
                        error!(
                            event = %event_name_clone,
                            at_block = &log.block_number,
                            error = %e,
                            "failed to invoke callback after retries"
                        );
                    }
                }
            });
            event_channels.insert(event_name, sender);
        }

        let mut stream = self.block_scanner.start().await;
        while let Some(range) = stream.next().await {
            match range {
                Ok(range) => {
                    let from_block = range.start;
                    let to_block = range.end;
                    info!(from_block, to_block, "processing placeholder block range");
                    self.process_block_range(from_block, to_block, &event_channels).await?;
                }
                Err(e) => {
                    error!(error = %e, "failed to get block range");
                }
            }
        }

        Ok(())
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
                            if let Err(e) = sender.send(log.clone()).await {
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

    async fn smart_retry(
        callback: &Arc<dyn crate::callback::EventCallback + Send + Sync>,
        log: &Log,
        config: &CallbackConfig,
    ) -> anyhow::Result<()> {
        match callback.on_event(log).await {
            Ok(()) => Ok(()),
            Err(first_err) => {
                // use exponential backoff for state-sync errors
                if is_missing_trie_node_error(&first_err) {
                    warn!(
                        error = %first_err,
                        "Detected missing trie node/state not available; using state-sync aware retry"
                    );

                    let mut delay = STATE_SYNC_RETRY_INTERVAL;
                    let start = tokio::time::Instant::now();

                    info!(
                        initial_interval = ?STATE_SYNC_RETRY_INTERVAL,
                        max_interval = ?STATE_SYNC_RETRY_MAX_INTERVAL,
                        max_elapsed = ?STATE_SYNC_RETRY_MAX_ELAPSED,
                        "Starting state-sync aware retry"
                    );

                    let mut last_err: anyhow::Error = first_err;
                    loop {
                        if start.elapsed() >= STATE_SYNC_RETRY_MAX_ELAPSED {
                            return Err(last_err);
                        }

                        tokio::time::sleep(delay).await;
                        match callback.on_event(log).await {
                            Ok(()) => return Ok(()),
                            Err(e) => {
                                last_err = e;
                                let next_secs = delay.as_secs_f64() * STATE_SYNC_RETRY_MULTIPLIER;
                                let next = Duration::from_secs_f64(next_secs);
                                delay = cmp::min(STATE_SYNC_RETRY_MAX_INTERVAL, next);
                                let elapsed = start.elapsed();
                                warn!(
                                    next_delay = ?delay,
                                    elapsed = ?elapsed,
                                    error = %last_err,
                                    "State-sync retry operation failed: will retry"
                                );
                            }
                        }
                    }
                } else {
                    let mut last_err: anyhow::Error = first_err;
                    for attempt in 1..=config.max_attempts {
                        warn!(
                            attempt,
                            max_attempts = config.max_attempts,
                            delay_ms = config.delay_ms,
                            "Callback failed: retrying after fixed delay"
                        );
                        tokio::time::sleep(Duration::from_millis(config.delay_ms)).await;
                        match callback.on_event(log).await {
                            Ok(()) => return Ok(()),
                            Err(e) => {
                                last_err = e;
                            }
                        }
                    }
                    Err(last_err)
                }
            }
        }
    }
}

fn is_missing_trie_node_error(err: &anyhow::Error) -> bool {
    let s = err.to_string().to_lowercase();
    s.contains("missing trie node") && s.contains("state") && s.contains("not available")
}
