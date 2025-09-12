#![allow(unused)]
use std::{collections::HashMap, future, sync::Arc, time::Duration};

use crate::{
    block_scanner::{BlockScanner, BlockScannerBuilder, OnBlocksFunc},
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
                    if let Err(e) = Self::invoke_with_retry_static(&callback, &log, &cfg).await {
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

        // TODO: replace with blockstream
        let from_block: u64 = 0;
        let to_block: u64 = 0;

        info!(from_block, to_block, "processing placeholder block range");
        self.process_block_range(from_block, to_block, &event_channels).await?;

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

    async fn invoke_with_retry_static(
        callback: &Arc<dyn crate::callback::EventCallback + Send + Sync>,
        log: &Log,
        config: &CallbackConfig,
    ) -> anyhow::Result<()> {
        let attempts = config.max_attempts.max(1);
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 1..=attempts {
            match callback.on_event(log).await {
                Ok(_) => return Ok(()),
                Err(e) => {
                    last_err = Some(e);
                    if attempt < attempts {
                        warn!(
                            attempt,
                            max_attempts = attempts,
                            "callback failed; retrying after fixed delay"
                        );
                        tokio::time::sleep(Duration::from_millis(config.delay_ms)).await;
                        continue;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("callback failed with unknown error")))
    }
}

// TODO: implement max channel buffer size test
