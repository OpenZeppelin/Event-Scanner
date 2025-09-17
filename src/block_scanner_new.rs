//! Example usage:
//!
//! ```rust,no_run
//! use alloy::{eips::BlockNumberOrTag, network::Ethereum, primitives::BlockNumber};
//! use event_scanner::block_scanner_new::SubscriptionError;
//! use std::ops::Range;
//! use tokio_stream::{StreamExt, wrappers::ReceiverStream};
//!
//! use alloy::transports::http::reqwest::Url;
//! use event_scanner::block_scanner_new::{BlockScanner, BlockScannerClient};
//! use tokio::time::Duration;
//! use tracing::{error, info};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Initialize logging
//!     tracing_subscriber::fmt::init();
//!
//!     // Configuration
//!     let block_scanner = BlockScanner::new()
//!         .with_blocks_read_per_epoch(1000)
//!         .with_reorg_rewind_depth(5)
//!         .with_retry_interval(Duration::from_secs(12))
//!         .with_block_confirmations(5)
//!         .connect_ws::<Ethereum>(Url::parse("ws://localhost:8546").unwrap());
//!
//!     // Create client to send subscribe command to block scanner
//!     let subscription_client: BlockScannerClient = block_scanner.run()?;
//!
//!     let mut receiver: ReceiverStream<Result<Range<BlockNumber>, SubscriptionError>> =
//!         subscription_client
//!             .subscribe(
//!                 BlockNumberOrTag::Latest,
//!                 None, // just subscribe to new blocks
//!                 1,    // wait until current blocks are processed before processing next range
//!             )
//!             .await?;
//!
//!     while let Some(result) = receiver.next().await {
//!         match result {
//!             Ok(range) => {
//!                 // process range
//!             }
//!             Err(e) => {
//!                 error!("Received error from subscription: {e}");
//!
//!                 // Decide whether to continue or break based on error type
//!                 match e {
//!                     SubscriptionError::ServiceShutdown => break,
//!                     SubscriptionError::WebSocketConnectionFailed(_) => {
//!                         // Maybe implement backoff and retry logic here
//!                         error!(
//!                             "WebSocket connection failed, continuing to listen for reconnection"
//!                         );
//!                     }
//!                     _ => {
//!                         // Continue processing for other errors
//!                         error!("Non-fatal error, continuing: {e}");
//!                     }
//!                 }
//!             }
//!         }
//!     }
//!
//!     info!("Data processing stopped.");
//!
//!     Ok(())
//! }
//! ```

use std::{marker::PhantomData, ops::Range, time::Duration};

use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;

use alloy::{
    consensus::BlockHeader,
    eips::{BlockId, BlockNumberOrTag},
    network::{BlockResponse, Network, primitives::HeaderResponse},
    primitives::{BlockHash, BlockNumber},
    providers::{Provider, RootProvider},
    pubsub::Subscription,
    rpc::client::ClientBuilder,
    transports::{
        TransportResult,
        http::reqwest::{self, Url},
        ws::WsConnect,
    },
};
use thiserror::Error;
use tracing::{debug, error, info, warn};

// copied form https://github.com/taikoxyz/taiko-mono/blob/f4b3a0e830e42e2fee54829326389709dd422098/packages/taiko-client/pkg/chain_iterator/block_batch_iterator.go#L19
const DEFAULT_BLOCKS_READ_PER_EPOCH: usize = 1000;
const DEFAULT_RETRY_INTERVAL: Duration = Duration::from_secs(12);
const DEFAULT_BLOCK_CONFIRMATIONS: u64 = 0;
// const BACK_OFF_MAX_RETRIES: u64 = 5;

const MAX_BUFFERED_MESSAGES: usize = 50000;

// TODO: determine check exact default value
const DEFAULT_REORG_REWIND_DEPTH: u64 = 0;

// // State sync aware retry settings
// const STATE_SYNC_RETRY_INTERVAL: Duration = Duration::from_secs(30);
// const STATE_SYNC_MAX_RETRIES: u64 = 12;

#[derive(Error, Debug)]
pub enum BlockScannerError {
    #[error("HTTP request failed: {0}")]
    HttpError(#[from] reqwest::Error),

    // #[error("WebSocket error: {0}")]
    // WebSocketError(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("RPC error: {0}")]
    RpcError(#[from] alloy::transports::RpcError<alloy::transports::TransportErrorKind>),

    #[error("Channel send error")]
    ChannelError,

    #[error("Service is shutting down")]
    ServiceShutdown,

    #[error("Only one subscriber allowed at a time")]
    MultipleSubscribers,

    #[error("Historical sync failed: {0}")]
    HistoricalSyncError(String),

    #[error("WebSocket connection failed after {0} attempts")]
    WebSocketConnectionFailed(usize),

    #[error("End of block batch")]
    Eof,
}

#[derive(Debug)]
pub enum Command {
    Subscribe {
        sender: mpsc::Sender<Result<Range<BlockNumber>, BlockScannerError>>,
        start_height: BlockNumberOrTag,
        end_height: Option<BlockNumberOrTag>,
        response: oneshot::Sender<Result<(), BlockScannerError>>,
    },
    Unsubscribe {
        response: oneshot::Sender<Result<(), BlockScannerError>>,
    },
    GetStatus {
        response: oneshot::Sender<ServiceStatus>,
    },
    Shutdown {
        response: oneshot::Sender<Result<(), BlockScannerError>>,
    },
}

#[derive(Debug, Clone)]
pub struct ServiceStatus {
    pub is_subscribed: bool,
    pub last_synced_block: Option<BlockHashAndNumber>,
    pub websocket_connected: bool,
    pub processed_count: u64,
    pub error_count: u64,
}

#[derive(Debug, Clone)]
pub struct BlockHashAndNumber {
    hash: BlockHash,
    number: BlockNumber,
}

impl BlockHashAndNumber {
    fn from_header<N: Network>(header: &N::HeaderResponse) -> Self {
        Self { hash: header.hash(), number: header.number() }
    }
}

#[derive(Clone)]
struct WsConnection<N: Network> {
    url: Url,
    network: PhantomData<N>,
}

#[derive(Clone)]
struct IpcConnection<N: Network> {
    path: String,
    network: PhantomData<N>,
}

#[derive(Clone)]
enum ConnectionType<N: Network> {
    Ws(WsConnection<N>),
    Ipc(IpcConnection<N>),
}

impl<N: Network> Connection<N> for ConnectionType<N> {
    async fn provider(&self) -> TransportResult<impl Provider<N> + Clone> {
        let client = match self {
            ConnectionType::Ws(ws) => {
                ClientBuilder::default().ws(WsConnect::new(ws.url.clone())).await?
            }
            ConnectionType::Ipc(ipc) => {
                ClientBuilder::default().ipc(ipc.path.clone().into()).await?
            }
        };
        Ok(RootProvider::<N>::new(client))
    }
}

trait Connection<N: Network>: Clone {
    async fn provider(&self) -> TransportResult<impl Provider<N> + Clone>;
}

#[derive(Clone)]
struct Config<N: Network> {
    connection: ConnectionType<N>,
    blocks_read_per_epoch: usize,
    reorg_rewind_depth: u64,
    #[allow(dead_code, reason = "TODO: will be used in smart retry mechanism")]
    retry_interval: Duration,
    #[allow(dead_code, reason = "TODO: will be used in reorg mechanism")]
    block_confirmations: u64,
}

pub struct BlockScanner {
    blocks_read_per_epoch: usize,
    reorg_rewind_depth: u64,
    retry_interval: Duration,
    block_confirmations: u64,
}

impl Default for BlockScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockScanner {
    #[must_use]
    pub fn new() -> Self {
        Self {
            blocks_read_per_epoch: DEFAULT_BLOCKS_READ_PER_EPOCH,
            reorg_rewind_depth: DEFAULT_REORG_REWIND_DEPTH,
            retry_interval: DEFAULT_RETRY_INTERVAL,
            block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS,
        }
    }

    #[must_use]
    pub fn with_blocks_read_per_epoch(&mut self, blocks_read_per_epoch: usize) -> &mut Self {
        self.blocks_read_per_epoch = blocks_read_per_epoch;
        self
    }

    #[must_use]
    pub fn with_reorg_rewind_depth(&mut self, reorg_rewind_depth: u64) -> &mut Self {
        self.reorg_rewind_depth = reorg_rewind_depth;
        self
    }

    #[must_use]
    pub fn with_retry_interval(&mut self, retry_interval: Duration) -> &mut Self {
        self.retry_interval = retry_interval;
        self
    }

    #[must_use]
    pub fn with_block_confirmations(&mut self, block_confirmations: u64) -> &mut Self {
        self.block_confirmations = block_confirmations;
        self
    }

    #[must_use]
    pub fn connect_ws<N: Network>(&self, ws_url: Url) -> ConnectedBlockScanner<N> {
        ConnectedBlockScanner {
            config: Config {
                connection: ConnectionType::Ws(WsConnection { url: ws_url, network: PhantomData }),
                blocks_read_per_epoch: self.blocks_read_per_epoch,
                reorg_rewind_depth: self.reorg_rewind_depth,
                retry_interval: self.retry_interval,
                block_confirmations: self.block_confirmations,
            },
        }
    }

    #[must_use]
    pub fn connect_ipc<N: Network>(&self, ipc_path: String) -> ConnectedBlockScanner<N> {
        ConnectedBlockScanner {
            config: Config {
                connection: ConnectionType::Ipc(IpcConnection {
                    path: ipc_path,
                    network: PhantomData,
                }),
                blocks_read_per_epoch: self.blocks_read_per_epoch,
                reorg_rewind_depth: self.reorg_rewind_depth,
                retry_interval: self.retry_interval,
                block_confirmations: self.block_confirmations,
            },
        }
    }
}

pub struct ConnectedBlockScanner<N: Network> {
    config: Config<N>,
}

impl<N: Network> ConnectedBlockScanner<N> {
    // TODO: use wrapper errors
    /// Returns the underlying Provider.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails.
    pub async fn provider(&self) -> TransportResult<impl Provider<N>> {
        self.config.connection.provider().await
    }

    /// Starts the subscription service and returns a client for sending commands.
    ///
    /// # Errors
    ///
    /// Returns an error if the subscription service fails to start.
    pub fn run(&self) -> anyhow::Result<BlockScannerClient> {
        let (service, cmd_tx) = BlockScannerService::new(self.config.clone());
        tokio::spawn(async move {
            service.run().await;
        });
        Ok(BlockScannerClient::new(cmd_tx))
    }
}

struct BlockScannerService<N: Network> {
    config: Config<N>,
    subscriber: Option<mpsc::Sender<Result<Range<BlockNumber>, BlockScannerError>>>,
    current: Option<BlockHashAndNumber>,
    websocket_connected: bool,
    processed_count: u64,
    error_count: u64,
    command_receiver: mpsc::Receiver<Command>,
    shutdown: bool,
}

impl<N: Network> BlockScannerService<N> {
    pub fn new(config: Config<N>) -> (Self, mpsc::Sender<Command>) {
        let (cmd_tx, cmd_rx) = mpsc::channel(100);

        let service = Self {
            config,
            subscriber: None,
            current: None,
            websocket_connected: false,
            processed_count: 0,
            error_count: 0,
            command_receiver: cmd_rx,
            shutdown: false,
        };

        (service, cmd_tx)
    }

    pub async fn run(mut self) {
        info!("Starting subscription service");

        while !self.shutdown {
            tokio::select! {
                cmd = self.command_receiver.recv() => {
                    if let Some(command) = cmd {
                        if let Err(e) = self.handle_command(command).await {
                            error!("Command handling error: {}", e);
                            self.error_count += 1;
                        }
                    } else {
                        info!("Command channel closed, shutting down");
                        break;
                    }
                }
            }
        }

        info!("Subscription service stopped");
    }

    async fn handle_command(&mut self, command: Command) -> Result<(), BlockScannerError> {
        match command {
            Command::Subscribe { sender, start_height, end_height, response } => {
                let result = self.handle_subscribe(sender, start_height, end_height).await;
                let _ = response.send(result);
            }
            Command::Unsubscribe { response } => {
                self.handle_unsubscribe();
                let _ = response.send(Ok(()));
            }
            Command::GetStatus { response } => {
                let status = self.get_status();
                let _ = response.send(status);
            }
            Command::Shutdown { response } => {
                self.shutdown = true;
                self.handle_unsubscribe();
                let _ = response.send(Ok(()));
            }
        }
        Ok(())
    }

    async fn handle_subscribe(
        &mut self,
        sender: mpsc::Sender<Result<Range<BlockNumber>, BlockScannerError>>,
        start_height: BlockNumberOrTag,
        end_height: Option<BlockNumberOrTag>,
    ) -> Result<(), BlockScannerError> {
        if self.subscriber.is_some() {
            return Err(BlockScannerError::MultipleSubscribers);
        }

        // TODO: update local state relate to reorg and validate data

        info!("Starting subscription from point: {start_height:?}");
        self.subscriber = Some(sender);

        self.sync_with_transition(start_height, end_height).await?;

        Ok(())
    }

    async fn sync_with_transition(
        &mut self,
        start_height: BlockNumberOrTag,
        end_height: Option<BlockNumberOrTag>,
    ) -> Result<(), BlockScannerError> {
        // Step 1: Establish WebSocket connection
        let (buffer_tx, buffer_rx) = mpsc::channel(MAX_BUFFERED_MESSAGES);

        let config = self.config.clone();
        let provider = config.connection.provider().await?;

        // Step 2: Perform historical sync
        let (start_block, sync_end_block) = if let Some(end_height) = end_height {
            let start_block =
                provider.get_block_by_number(start_height).await?.expect("already checked");
            let end_block =
                provider.get_block(end_height.into()).await?.expect("TODO: check if really valid");
            (start_block, end_block)
        } else {
            let start_block =
                provider.get_block_by_number(start_height).await?.expect("already checked");
            let end_block = provider
                .get_block(BlockId::Number(BlockNumberOrTag::Latest))
                .await?
                .expect("TODO: check if really valid");
            (start_block, end_block)
        };

        info!(
            "Syncing historical data from {} to {}",
            start_block.header().number(),
            sync_end_block.header().number()
        );

        // start buffering the subscription data
        let connection_clone = self.config.connection.clone();
        let cutoff = sync_end_block.header().number();
        let ws_task = tokio::spawn(async move {
            if end_height.is_none() {
                Self::websocket_buffer_task(cutoff, connection_clone, buffer_tx).await;
            }
        });

        // TODO: invoke with smart retry mechanism with backoff
        if let Err(e) = self.sync_historical_data(&provider, start_block, sync_end_block).await {
            warn!("aborting ws_task");
            ws_task.abort();
            return Err(BlockScannerError::HistoricalSyncError(e.to_string()));
        }

        // Step 3: Process buffered WebSocket messages
        let sender = self.subscriber.clone().expect("subscriber should be set");
        tokio::spawn(async move {
            if end_height.is_none() {
                Self::process_buffered_messages(buffer_rx, sender, cutoff).await;
            } else if sender.send(Err(BlockScannerError::Eof)).await.is_err() {
                warn!("Subscriber channel closed, cleaning up");
            }
        });

        if end_height.is_none() {
            info!("Successfully transitioned from historical to live data");
        } else {
            info!("Successfully synced historical data");
        }

        Ok(())
    }

    async fn sync_historical_data<P: Provider<N>>(
        &mut self,
        provider: &P,
        start: N::BlockResponse,
        end: N::BlockResponse,
    ) -> Result<(), BlockScannerError> {
        let mut batch_count = 0;

        self.current = Some(BlockHashAndNumber::from_header::<N>(start.header()));

        while self.current.as_ref().unwrap().number < end.header().number() {
            self.ensure_current_not_reorged(provider).await?;

            let batch_to = if self.current.as_ref().unwrap().number
                + self.config.blocks_read_per_epoch as u64
                > end.header().number()
            {
                end.header().number()
            } else {
                self.current.as_ref().unwrap().number + self.config.blocks_read_per_epoch as u64
            };

            let batch_end_block = provider
                .get_block_by_number(batch_to.into())
                .await?
                .expect("TODO: check if really valid");

            self.send_to_subscriber(Ok(self.current.as_ref().unwrap().number..batch_to)).await;

            self.current = Some(BlockHashAndNumber::from_header::<N>(batch_end_block.header()));

            batch_count += 1;
            if batch_count % 10 == 0 {
                debug!("Processed {batch_count} historical batches");
            }
        }

        info!("Historical sync completed: {batch_count} batches processed");
        Ok(())
    }

    async fn ensure_current_not_reorged<P: Provider<N>>(
        &mut self,
        provider: &P,
    ) -> Result<(), BlockScannerError> {
        let current_block = provider.get_block_by_hash(self.current.as_ref().unwrap().hash).await?;
        if current_block.is_some() {
            return Ok(());
        }

        self.rewind_on_reorg_detected(provider).await
    }

    async fn rewind_on_reorg_detected<P: Provider<N>>(
        &mut self,
        provider: P,
    ) -> Result<(), BlockScannerError> {
        let mut new_current_height =
            if self.current.as_ref().unwrap().number <= self.config.reorg_rewind_depth {
                0
            } else {
                self.current.as_ref().unwrap().number - self.config.reorg_rewind_depth
            };

        let head = provider.get_block_number().await?;
        if head < new_current_height {
            new_current_height = head;
        }

        let current = provider
            .get_block_by_number(new_current_height.into())
            .await?
            .map(|block| BlockHashAndNumber::from_header::<N>(block.header()))
            .expect("block should exist");

        info!(
            "Rewind on reorg detected\noldCurrent: {}, newCurrent: {}",
            self.current.as_ref().unwrap().number,
            current.number
        );

        self.current = Some(current);

        Ok(())
    }

    async fn websocket_buffer_task(
        mut current: BlockNumber,
        connection: ConnectionType<N>,
        buffer_sender: mpsc::Sender<Range<BlockNumber>>,
    ) {
        // TODO: consider passing errors to the caller for handling
        let provider = match connection.provider().await {
            Ok(p) => p,
            Err(e) => {
                error!("Failed to instantiate provider: {e}");
                return;
            }
        };

        // TODO: use smart retry mechanism
        match Self::get_block_subscription(&provider).await {
            Ok(mut ws_stream) => {
                info!("WebSocket connected for buffering");

                // TODO: if latest != ws_stream.next(), then return latest.number and empty the ws_stream backlog
                while let Ok(header_resp) = ws_stream.recv().await {
                    info!("Received block header: {}", header_resp.number());
                    // TODO: handle reorgs
                    if current == header_resp.number() {
                        continue;
                    }

                    // we add 1 to include the latest block
                    #[allow(clippy::range_plus_one)]
                    if let Err(e) = buffer_sender.send(current..header_resp.number() + 1).await {
                        error!("Buffer channel closed, stopping buffer task: {e}");

                        return;
                    }

                    // next block will be processed in the next batch
                    current = header_resp.number() + 1;
                }
            }
            Err(e) => {
                error!("Failed to connect WebSocket for buffering: {e}");
            }
        }
    }

    async fn process_buffered_messages(
        mut buffer_rx: mpsc::Receiver<Range<BlockNumber>>,
        sender: mpsc::Sender<Result<Range<BlockNumber>, BlockScannerError>>,
        cutoff: BlockNumber,
    ) {
        let mut processed = 0;
        let mut discarded = 0;

        // Process all buffered messages
        while let Some(range) = buffer_rx.recv().await {
            let (start, end) = (range.start, range.end);
            if start >= cutoff {
                if sender.send(Ok(range)).await.is_err() {
                    warn!("Subscriber channel closed, cleaning up");
                    return;
                }
                processed += end - start;
            } else if end > cutoff {
                // TODO: verify the math
                discarded += cutoff - start;

                let start = cutoff;
                if sender.send(Ok(start..end)).await.is_err() {
                    warn!("Subscriber channel closed, cleaning up");
                    return;
                }
                processed += end - start;
            } else {
                discarded += end - start;
            }
        }

        info!("Processed buffered messages: {processed} forwarded, {discarded} discarded");
    }

    async fn get_block_subscription(
        provider: &impl Provider<N>,
    ) -> Result<Subscription<N::HeaderResponse>, BlockScannerError> {
        let ws_stream = provider
            .subscribe_blocks()
            .await
            .map_err(|_| BlockScannerError::WebSocketConnectionFailed(1))?;

        Ok(ws_stream)
    }

    async fn send_to_subscriber(&mut self, result: Result<Range<BlockNumber>, BlockScannerError>) {
        if let Some(ref sender) = self.subscriber {
            if sender.send(result).await.is_err() {
                self.subscriber = None;
                self.websocket_connected = false;
            } else {
                self.processed_count += 1;
            }
        }
    }

    fn handle_unsubscribe(&mut self) {
        if self.subscriber.take().is_some() {
            info!("Unsubscribing current subscriber");
            self.websocket_connected = false;
        }
    }

    fn get_status(&self) -> ServiceStatus {
        ServiceStatus {
            is_subscribed: self.subscriber.is_some(),
            last_synced_block: self.current.clone(),
            websocket_connected: self.websocket_connected,
            processed_count: self.processed_count,
            error_count: self.error_count,
        }
    }
}

pub struct BlockScannerClient {
    command_sender: mpsc::Sender<Command>,
}

impl BlockScannerClient {
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
    /// * `SubscriptionError::ServiceShutdown` - if the service is already shutting down.
    pub async fn subscribe(
        &self,
        start_height: BlockNumberOrTag,
        end_height: Option<BlockNumberOrTag>,
    ) -> Result<ReceiverStream<Result<Range<BlockNumber>, BlockScannerError>>, BlockScannerError>
    {
        let (blocks_sender, blocks_receiver) = mpsc::channel(MAX_BUFFERED_MESSAGES);
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::Subscribe {
            sender: blocks_sender,
            start_height,
            end_height,
            response: response_tx,
        };

        self.command_sender.send(command).await.map_err(|_| BlockScannerError::ServiceShutdown)?;

        response_rx.await.map_err(|_| BlockScannerError::ServiceShutdown)??;

        let stream = ReceiverStream::new(blocks_receiver);

        Ok(stream)
    }

    /// Unsubscribes the current subscriber.
    ///
    /// # Errors
    ///
    /// * `SubscriptionError::ServiceShutdown` - if the service is already shutting down.
    pub async fn unsubscribe(&self) -> Result<(), BlockScannerError> {
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::Unsubscribe { response: response_tx };

        self.command_sender.send(command).await.map_err(|_| BlockScannerError::ServiceShutdown)?;

        response_rx.await.map_err(|_| BlockScannerError::ServiceShutdown)?
    }

    /// Returns the current status of the subscription service.
    ///
    /// # Errors
    ///
    /// * `SubscriptionError::ServiceShutdown` - if the service is already shutting down.
    pub async fn get_status(&self) -> Result<ServiceStatus, BlockScannerError> {
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::GetStatus { response: response_tx };

        self.command_sender.send(command).await.map_err(|_| BlockScannerError::ServiceShutdown)?;

        response_rx.await.map_err(|_| BlockScannerError::ServiceShutdown)
    }

    /// Shuts down the subscription service and unsubscribes the current subscriber.
    ///
    /// # Errors
    ///
    /// * `SubscriptionError::ServiceShutdown` - if the service is already shutting down.
    pub async fn shutdown(&self) -> Result<(), BlockScannerError> {
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::Shutdown { response: response_tx };

        self.command_sender.send(command).await.map_err(|_| BlockScannerError::ServiceShutdown)?;

        response_rx.await.map_err(|_| BlockScannerError::ServiceShutdown)?
    }
}

#[cfg(test)]
mod tests {
    use alloy::network::Ethereum;
    use alloy_node_bindings::Anvil;

    use tokio_stream::StreamExt;

    use super::*;

    #[tokio::test]
    async fn live_mode_processes_all_blocks() -> anyhow::Result<()> {
        let anvil = Anvil::new().block_time_f64(0.01).try_spawn()?;

        let sub_client = BlockScanner::new()
            .with_blocks_read_per_epoch(3)
            .with_reorg_rewind_depth(5)
            .with_retry_interval(Duration::from_secs(1))
            .with_block_confirmations(1)
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .run()?;

        let expected_blocks = 10;

        let mut receiver =
            sub_client.subscribe(BlockNumberOrTag::Latest, None).await?.take(expected_blocks);

        let mut block_range_start = 0;

        while let Some(result) = receiver.next().await {
            match result {
                Ok(range) => {
                    println!("Received block range: {} - {}", range.start, range.end);
                    if block_range_start == 0 {
                        block_range_start = range.start;
                    }

                    assert_eq!(block_range_start, range.start);
                    assert!(range.end >= range.start);
                    block_range_start = range.end;
                }
                Err(e) => {
                    panic!("Received error from subscription: {e}");
                }
            }
        }

        Ok(())
    }
}
