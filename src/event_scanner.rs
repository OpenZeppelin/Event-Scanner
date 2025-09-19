use std::{collections::HashMap, ops::Range, sync::Arc, time::Duration};

use crate::{
    block_range_scanner::{
        self, BlockRangeScanner, BlockScannerClient, BlockScannerError, BlockScannerService,
        ConnectedBlockScanner,
    },
    callback::strategy::{CallbackStrategy, StateSyncAwareStrategy},
    types::EventFilter,
};
use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    primitives::{Address, BlockNumber},
    providers::{Provider, RootProvider},
    rpc::types::{Filter, Log},
    transports::http::reqwest::Url,
};
use tokio::sync::{
    mpsc::{self, Receiver},
    oneshot,
};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tracing::{error, info, warn};

#[derive(Debug)]
pub enum Command {
    Subscribe {
        sender: mpsc::Sender<Result<Vec<Log>, BlockScannerError>>,
        start_height: BlockNumberOrTag,
        end_height: Option<BlockNumberOrTag>,
        response: oneshot::Sender<Result<(), BlockScannerError>>,
    },
    Unsubscribe {
        response: oneshot::Sender<Result<(), BlockScannerError>>,
    },
    Shutdown {
        response: oneshot::Sender<Result<(), BlockScannerError>>,
    },
}

pub struct EventScanner {
    block_range_scanner: BlockRangeScanner,
    tracked_events: Vec<EventFilter>,
    callback_strategy: Arc<dyn CallbackStrategy>,
}

impl Default for EventScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl EventScanner {
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

    /// Adjusts the retry interval when reconnecting to the provider.
    #[must_use]
    pub fn with_retry_interval(mut self, retry_interval: Duration) -> Self {
        self.block_range_scanner = self.block_range_scanner.with_retry_interval(retry_interval);
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
    ) -> Result<ConnectedEventScanner<N>, BlockScannerError> {
        Ok(ConnectedEventScanner {
            block_range_scanner: self.block_range_scanner.connect_ws(ws_url).await?,
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
    ) -> Result<ConnectedEventScanner<N>, BlockScannerError> {
        Ok(ConnectedEventScanner {
            block_range_scanner: self.block_range_scanner.connect_ipc(ipc_path.into()).await?,
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

pub struct ConnectedEventScanner<N: Network> {
    block_range_scanner: ConnectedBlockScanner<N>,
    tracked_events: Vec<EventFilter>,
    callback_strategy: Arc<dyn CallbackStrategy>,
}

impl<N: Network> ConnectedEventScanner<N> {
    /// Returns the underlying Provider.
    #[must_use]
    pub fn provider(&self) -> &RootProvider<N> {
        self.block_range_scanner.provider()
    }

    // TODO: use wrapper errors
    /// Starts the subscription service and returns a client for sending commands.
    ///
    /// # Errors
    ///
    /// Returns an error if the subscription service fails to start.
    pub fn run(self) -> anyhow::Result<EventScannerClient> {
        let block_range_client = self.block_range_scanner.run()?;
        let (service, cmd_tx) = EventScannerService::new(self, block_range_client);
        tokio::spawn(async move {
            service.run().await;
        });
        Ok(EventScannerClient::new(cmd_tx))
    }
}

#[derive(Hash, Eq, PartialEq)]
struct EventIdentifier {
    contract_address: Address,
    event: String,
}

struct EventScannerService<N: Network> {
    block_range_client: BlockScannerClient,
    provider: RootProvider<N>,
    tracked_events: Vec<EventFilter>,
    callback_strategy: Arc<dyn CallbackStrategy>,
    command_receiver: mpsc::Receiver<Command>,
    shutdown: bool,
}

impl<N: Network> EventScannerService<N> {
    pub fn new(
        connected_scanner: ConnectedEventScanner<N>,
        block_range_client: BlockScannerClient,
    ) -> (Self, mpsc::Sender<Command>) {
        let (cmd_tx, cmd_rx) = mpsc::channel(100);

        let service = Self {
            block_range_client,
            provider: connected_scanner.provider().clone(),
            callback_strategy: connected_scanner.callback_strategy,
            command_receiver: cmd_rx,
            tracked_events: connected_scanner.tracked_events,
            shutdown: false,
        };

        (service, cmd_tx)
    }

    pub async fn run(mut self) {
        info!("Starting event scanner service");

        while !self.shutdown {
            tokio::select! {
                cmd = self.command_receiver.recv() => {
                    if let Some(command) = cmd {
                        if let Err(e) = self.handle_command(command).await {
                            error!("Command handling error: {}", e);
                        }
                    } else {
                        info!("Command channel closed, shutting down");
                        break;
                    }
                }
            }
        }

        info!("Event scanner service stopped");
    }

    async fn handle_command(&mut self, command: Command) -> Result<(), BlockScannerError> {
        match command {
            Command::Subscribe { sender, start_height, end_height, response } => {
                let result = self.handle_subscribe(sender, start_height, end_height).await;
                let _ = response.send(result);
            }
            Command::Unsubscribe { response: _ } => {}
            Command::Shutdown { response: _ } => {}
        }
        Ok(())
    }

    /// Starts the scanner
    ///
    /// # Errors
    ///
    /// Returns an error if the scanner fails to start
    async fn handle_subscribe(
        &mut self,
        _sender: mpsc::Sender<Result<Vec<Log>, BlockScannerError>>,
        start_height: BlockNumberOrTag,
        end_height: Option<BlockNumberOrTag>,
    ) -> Result<(), BlockScannerError> {
        let mut event_channels: HashMap<EventIdentifier, mpsc::Sender<Log>> = HashMap::new();

        for filter in &self.tracked_events {
            let unique_event = EventIdentifier {
                contract_address: filter.contract_address,
                event: filter.event.clone(),
            };

            if event_channels.contains_key(&unique_event) {
                continue;
            }

            // TODO: configurable buffer size / smaller buffer ?
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

        let mut stream = self.block_range_client.subscribe(start_height, end_height).await?;

        while let Some(range) = stream.next().await {
            match range {
                Ok(range) => {
                    let from_block = range.start;
                    let to_block = range.end;
                    info!(from_block, to_block, "processing block range");
                    self.process_block_range(from_block, to_block, &event_channels).await?;
                }
                Err(BlockScannerError::Eof) => {
                    info!("End of block batch");
                    break;
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

    /// Fetches logs for the supplied block range and forwards them to the callback channels.
    async fn process_block_range(
        &self,
        from_block: u64,
        to_block: u64,
        event_channels: &HashMap<EventIdentifier, mpsc::Sender<Log>>,
    ) -> Result<(), BlockScannerError> {
        for event_filter in &self.tracked_events {
            let filter = Filter::new()
                .address(event_filter.contract_address)
                .event(event_filter.event.as_str())
                .from_block(from_block)
                .to_block(to_block);

            match self.provider.get_logs(&filter).await {
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

pub struct EventScannerClient {
    command_sender: mpsc::Sender<Command>,
}

impl EventScannerClient {
    /// Creates a new subscription client.
    ///
    /// # Arguments
    ///
    /// * `command_sender` - The sender for sending commands to the subscription service.
    #[must_use]
    pub fn new(command_sender: mpsc::Sender<Command>) -> Self {
        Self { command_sender }
    }

    /// Subscribes to new blocks.
    ///
    /// # Arguments
    ///
    /// * `start_height` - The block number to start from.
    /// * `end_height` - The block number to end at.
    ///
    /// # Errors
    ///
    /// * `BlockScannerError::ServiceShutdown` - if the service is already shutting down.
    pub async fn subscribe(
        &self,
        start_height: BlockNumberOrTag,
        end_height: Option<BlockNumberOrTag>,
    ) -> Result<ReceiverStream<Result<Vec<Log>, BlockScannerError>>, BlockScannerError> {
        let (event_sender, event_receiver) = mpsc::channel(100);
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::Subscribe {
            sender: event_sender,
            start_height,
            end_height,
            response: response_tx,
        };

        self.command_sender.send(command).await.map_err(|_| BlockScannerError::ServiceShutdown)?;

        response_rx.await.map_err(|_| BlockScannerError::ServiceShutdown)??;

        let stream = ReceiverStream::new(event_receiver);

        Ok(stream)
    }

    /// Unsubscribes the current subscriber.
    ///
    /// # Errors
    ///
    /// * `BlockScannerError::ServiceShutdown` - if the service is already shutting down.
    pub async fn unsubscribe(&self) -> Result<(), BlockScannerError> {
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::Unsubscribe { response: response_tx };

        self.command_sender.send(command).await.map_err(|_| BlockScannerError::ServiceShutdown)?;

        response_rx.await.map_err(|_| BlockScannerError::ServiceShutdown)?
    }

    /// Shuts down the subscription service and unsubscribes the current subscriber.
    ///
    /// # Errors
    ///
    /// * `BlockScannerError::ServiceShutdown` - if the service is already shutting down.
    pub async fn shutdown(&self) -> Result<(), BlockScannerError> {
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::Shutdown { response: response_tx };

        self.command_sender.send(command).await.map_err(|_| BlockScannerError::ServiceShutdown)?;

        response_rx.await.map_err(|_| BlockScannerError::ServiceShutdown)?
    }
}
