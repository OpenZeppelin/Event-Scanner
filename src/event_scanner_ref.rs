use crate::{
    block_range_scanner_ref::{
        BlockRangeScanner, BlockScannerClient, BlockScannerError, ConnectedBlockScanner,
    },
    types::{ConnectionId, EventFilterRef},
};
use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    primitives::map::HashMap,
    providers::{Provider, RootProvider},
    rpc::types::{Filter, Log},
    transports::http::reqwest::Url,
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tracing::{error, info, warn};

#[derive(Debug)]
pub enum Command {
    Subscribe {
        sender: mpsc::Sender<Result<Vec<Log>, BlockScannerError>>,
        tracked_event: EventFilterRef,
        response: oneshot::Sender<Result<ConnectionId, BlockScannerError>>,
    },
    Sync {
        sender: mpsc::Sender<Result<Vec<Log>, BlockScannerError>>,
        tracked_event: EventFilterRef,
        from_block: BlockNumberOrTag,
        response: oneshot::Sender<Result<(), BlockScannerError>>,
    },
    Fetch {
        sender: mpsc::Sender<Result<Vec<Log>, BlockScannerError>>,
        tracked_event: EventFilterRef,
        from_block: BlockNumberOrTag,
        to_block: BlockNumberOrTag,
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
        Self { block_range_scanner: BlockRangeScanner::new() }
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
    ) -> Result<ConnectedEventScanner<N>, BlockScannerError> {
        Ok(ConnectedEventScanner {
            block_range_scanner: self.block_range_scanner.connect_ws(ws_url).await?,
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
        })
    }
}

pub struct ConnectedEventScanner<N: Network> {
    block_range_scanner: ConnectedBlockScanner<N>,
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
        let provider = self.provider().clone();

        let (service, cmd_tx) = EventScannerService::new(provider, block_range_client);
        tokio::spawn(async move {
            service.run().await;
        });

        Ok(EventScannerClient::new(cmd_tx))
    }
}

struct EventScannerService<N: Network> {
    block_range_client: BlockScannerClient,
    provider: RootProvider<N>,
    command_receiver: mpsc::Receiver<Command>,
    connections:
        HashMap<ConnectionId, (mpsc::Sender<Result<Vec<Log>, BlockScannerError>>, JoinHandle<()>)>,
    shutdown: bool,
}

impl<N: Network> EventScannerService<N> {
    pub fn new(
        provider: RootProvider<N>,
        block_range_client: BlockScannerClient,
    ) -> (Self, mpsc::Sender<Command>) {
        let (cmd_tx, cmd_rx) = mpsc::channel(100);

        let service = Self {
            block_range_client,
            provider,
            command_receiver: cmd_rx,
            connections: HashMap::default(),
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
            Command::Subscribe { sender, tracked_event, response } => {
                let result = self.handle_subscribe(sender, tracked_event).await;
                response.send(result).expect("response channel should be open");
            }
            _ => {}
        }
        Ok(())
    }

    /// Starts the subscription service.
    ///
    /// # Errors
    ///
    /// Returns an error if the scanner fails to start
    async fn handle_subscribe(
        &mut self,
        sender: mpsc::Sender<Result<Vec<Log>, BlockScannerError>>,
        filter: EventFilterRef,
    ) -> Result<ConnectionId, BlockScannerError> {
        let connection_id = (self.connections.len() + 1) as ConnectionId;

        info!("Starting subscription for event filter: {filter:?}");

        let join_handle = self.spawn_event_scanner(sender.clone(), filter).await?;

        self.connections.insert(connection_id, (sender, join_handle));

        Ok(connection_id)
    }

    /// Spawns background tasks that drive callback execution for an event type.
    async fn spawn_event_scanner(
        &mut self,
        sender: mpsc::Sender<Result<Vec<Log>, BlockScannerError>>,
        filter: EventFilterRef,
    ) -> Result<JoinHandle<()>, BlockScannerError> {
        let provider = self.provider.clone();

        let mut stream = self.block_range_client.subscribe(BlockNumberOrTag::Latest, None).await?;

        let join_handle = tokio::spawn(async move {
            while let Some(range) = stream.next().await {
                match range {
                    Ok(range) => {
                        let from_block = range.start;
                        let to_block = range.end;
                        info!(from_block, to_block, "processing block range");
                        if let Err(e) = Self::process_block_range(
                            sender.clone(),
                            &provider,
                            &filter,
                            from_block,
                            to_block,
                        )
                        .await
                        {
                            error!(error = %e, "failed to process block range");

                            sender.send(Err(e)).await.expect("receiver should be open");
                        }
                    }
                    Err(BlockScannerError::Eof) => {
                        info!("End of block batch");
                        sender
                            .send(Err(BlockScannerError::Eof))
                            .await
                            .expect("receiver should be open");
                        break;
                    }
                    Err(e) => {
                        error!(error = %e, "failed to get block range");
                        sender.send(Err(e)).await.expect("receiver should be open");
                    }
                }
            }
        });

        Ok(join_handle)
    }

    /// Fetches logs for the supplied block range and forwards them to the callback channels.
    async fn process_block_range(
        sender: mpsc::Sender<Result<Vec<Log>, BlockScannerError>>,
        provider: &RootProvider<N>,
        event_filter: &EventFilterRef,
        from_block: u64,
        to_block: u64,
    ) -> Result<(), BlockScannerError> {
        let filter = Filter::new()
            .address(event_filter.contract_address)
            .event(event_filter.event.as_str())
            .from_block(from_block)
            .to_block(to_block);

        match provider.get_logs(&filter).await {
            Ok(logs) => {
                if logs.is_empty() {
                    return Ok(());
                }
                info!(
                    contract = ?event_filter.contract_address,
                    event = %event_filter.event,
                    log_count = logs.len(),
                    from_block,
                    to_block,
                    "found logs for event in block range"
                );

                if let Err(e) = sender.send(Ok(logs)).await {
                    warn!(event = %event_filter.event, error = %e, "failed to enqueue log for processing");
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
        event_filter: EventFilterRef,
    ) -> Result<
        (ConnectionId, ReceiverStream<Result<Vec<Log>, BlockScannerError>>),
        BlockScannerError,
    > {
        let (event_sender, event_receiver) = mpsc::channel(100);
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::Subscribe {
            sender: event_sender,
            tracked_event: event_filter,
            response: response_tx,
        };

        self.command_sender.send(command).await.map_err(|_| BlockScannerError::ServiceShutdown)?;

        // TODO: consider different error name
        let connection_id = response_rx.await.map_err(|_| BlockScannerError::ServiceShutdown)??;

        let stream = ReceiverStream::new(event_receiver);

        Ok((connection_id, stream))
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
