//! Example usage:
//!
//! ```rust,no_run
//! use alloy::{eips::BlockNumberOrTag, network::Ethereum, primitives::BlockNumber};
//! use std::ops::RangeInclusive;
//! use tokio_stream::{StreamExt, wrappers::ReceiverStream};
//!
//! use alloy::transports::http::reqwest::Url;
//! use event_scanner::block_range_scanner::{
//!     BlockRangeMessage, BlockRangeScanner, BlockRangeScannerClient, BlockRangeScannerError,
//!     DEFAULT_BLOCK_CONFIRMATIONS, DEFAULT_BLOCKS_READ_PER_EPOCH,
//! };
//! use tokio::time::Duration;
//! use tracing::{error, info};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Initialize logging
//!     tracing_subscriber::fmt::init();
//!
//!     // Configuration
//!     let block_range_scanner = BlockRangeScanner::new()
//!         .connect_ws::<Ethereum>(Url::parse("ws://localhost:8546").unwrap())
//!         .await?;
//!
//!     // Create client to send subscribe command to block scanner
//!     let client: BlockRangeScannerClient = block_range_scanner.run()?;
//!
//!     let mut stream =
//!         client.stream_from(BlockNumberOrTag::Number(5), DEFAULT_BLOCK_CONFIRMATIONS).await?;
//!
//!     while let Some(message) = stream.next().await {
//!         match message {
//!             BlockRangeMessage::Data(range) => {
//!                 // process range
//!             }
//!             BlockRangeMessage::Error(e) => {
//!                 error!("Received error from subscription: {e}");
//!                 match e {
//!                     BlockRangeScannerError::ServiceShutdown => break,
//!                     BlockRangeScannerError::WebSocketConnectionFailed(_) => {
//!                         error!(
//!                             "WebSocket connection failed, continuing to listen for reconnection"
//!                         );
//!                     }
//!                     _ => {
//!                         error!("Non-fatal error, continuing: {e}");
//!                     }
//!                 }
//!             }
//!             BlockRangeMessage::Status(status) => {
//!                 info!("Received status message: {:?}", status);
//!             }
//!         }
//!     }
//!
//!     info!("Data processing stopped.");
//!
//!     Ok(())
//! }
//! ```

use std::{ops::RangeInclusive, sync::Arc};

use tokio::sync::{mpsc, oneshot};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};

use crate::types::{ScannerMessage, ScannerStatus};
use alloy::{
    consensus::BlockHeader,
    eips::BlockNumberOrTag,
    network::{BlockResponse, Network, primitives::HeaderResponse},
    primitives::{BlockHash, BlockNumber},
    providers::{Provider, RootProvider},
    pubsub::Subscription,
    rpc::client::ClientBuilder,
    transports::{
        RpcError, TransportErrorKind, TransportResult,
        http::reqwest::{self, Url},
        ws::WsConnect,
    },
};
use thiserror::Error;
use tracing::{debug, error, info, warn};

pub const DEFAULT_BLOCKS_READ_PER_EPOCH: usize = 1000;
// copied form https://github.com/taikoxyz/taiko-mono/blob/f4b3a0e830e42e2fee54829326389709dd422098/packages/taiko-client/pkg/chain_iterator/block_batch_iterator.go#L19
pub const DEFAULT_BLOCK_CONFIRMATIONS: u64 = 0;
// const BACK_OFF_MAX_RETRIES: u64 = 5;

pub const MAX_BUFFERED_MESSAGES: usize = 50000;

pub type BlockRangeMessage = ScannerMessage<RangeInclusive<BlockNumber>, BlockRangeScannerError>;

#[derive(Error, Debug, Clone)]
pub enum BlockRangeScannerError {
    #[error("HTTP request failed: {0}")]
    HttpError(Arc<reqwest::Error>),

    // #[error("WebSocket error: {0}")]
    // WebSocketError(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("Serialization error: {0}")]
    SerializationError(Arc<serde_json::Error>),

    #[error("RPC error: {0}")]
    RpcError(Arc<RpcError<TransportErrorKind>>),

    #[error("Channel send error")]
    ChannelError,

    #[error("Service is shutting down")]
    ServiceShutdown,

    #[error("Only one subscriber allowed at a time")]
    MultipleSubscribers,

    #[error("No subscriber set for streaming")]
    NoSubscriber,

    #[error("Historical sync failed: {0}")]
    HistoricalSyncError(String),

    #[error("WebSocket connection failed after {0} attempts")]
    WebSocketConnectionFailed(usize),

    #[error("Block not found, block number: {0}")]
    BlockNotFound(BlockNumberOrTag),
}

impl From<reqwest::Error> for BlockRangeScannerError {
    fn from(error: reqwest::Error) -> Self {
        BlockRangeScannerError::HttpError(Arc::new(error))
    }
}

impl From<serde_json::Error> for BlockRangeScannerError {
    fn from(error: serde_json::Error) -> Self {
        BlockRangeScannerError::SerializationError(Arc::new(error))
    }
}

impl From<RpcError<TransportErrorKind>> for BlockRangeScannerError {
    fn from(error: RpcError<TransportErrorKind>) -> Self {
        BlockRangeScannerError::RpcError(Arc::new(error))
    }
}

#[derive(Default, Debug, Clone)]
pub struct BlockHashAndNumber {
    pub hash: BlockHash,
    pub number: BlockNumber,
}

impl BlockHashAndNumber {
    fn from_header<N: Network>(header: &N::HeaderResponse) -> Self {
        Self { hash: header.hash(), number: header.number() }
    }
}

#[derive(Clone, Copy)]
pub struct BlockRangeScanner {
    pub max_read_per_epoch: usize,
}

impl Default for BlockRangeScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockRangeScanner {
    #[must_use]
    pub fn new() -> Self {
        Self { max_read_per_epoch: DEFAULT_BLOCKS_READ_PER_EPOCH }
    }

    #[must_use]
    pub fn with_max_read_per_epoch(mut self, max_read_per_epoch: usize) -> Self {
        self.max_read_per_epoch = max_read_per_epoch;
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
    ) -> TransportResult<ConnectedBlockRangeScanner<N>> {
        let provider =
            RootProvider::<N>::new(ClientBuilder::default().ws(WsConnect::new(ws_url)).await?);
        self.connect_provider(provider)
    }

    /// Connects to the provider via IPC
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<ConnectedBlockRangeScanner<N>> {
        let provider = RootProvider::<N>::new(ClientBuilder::default().ipc(ipc_path.into()).await?);
        self.connect_provider(provider)
    }

    /// Connects to an existing provider
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub fn connect_provider<N: Network>(
        self,
        provider: RootProvider<N>,
    ) -> TransportResult<ConnectedBlockRangeScanner<N>> {
        Ok(ConnectedBlockRangeScanner { provider, max_read_per_epoch: self.max_read_per_epoch })
    }
}

pub struct ConnectedBlockRangeScanner<N: Network> {
    provider: RootProvider<N>,
    max_read_per_epoch: usize,
}

impl<N: Network> ConnectedBlockRangeScanner<N> {
    /// Returns the underlying Provider.
    #[must_use]
    pub fn provider(&self) -> &RootProvider<N> {
        &self.provider
    }

    /// Starts the subscription service and returns a client for sending commands.
    ///
    /// # Errors
    ///
    /// Returns an error if the subscription service fails to start.
    pub fn run(&self) -> Result<BlockRangeScannerClient, BlockRangeScannerError> {
        let (service, cmd_tx) = Service::new(self.provider.clone(), self.max_read_per_epoch);
        tokio::spawn(async move {
            service.run().await;
        });
        Ok(BlockRangeScannerClient::new(cmd_tx))
    }
}

#[derive(Debug)]
pub enum Command {
    StreamLive {
        sender: mpsc::Sender<BlockRangeMessage>,
        block_confirmations: u64,
        response: oneshot::Sender<Result<(), BlockRangeScannerError>>,
    },
    StreamHistorical {
        sender: mpsc::Sender<BlockRangeMessage>,
        start_height: BlockNumberOrTag,
        end_height: BlockNumberOrTag,
        response: oneshot::Sender<Result<(), BlockRangeScannerError>>,
    },
    StreamFrom {
        sender: mpsc::Sender<BlockRangeMessage>,
        start_height: BlockNumberOrTag,
        block_confirmations: u64,
        response: oneshot::Sender<Result<(), BlockRangeScannerError>>,
    },
    Unsubscribe {
        response: oneshot::Sender<Result<(), BlockRangeScannerError>>,
    },
    Shutdown {
        response: oneshot::Sender<Result<(), BlockRangeScannerError>>,
    },
}

struct Service<N: Network> {
    provider: RootProvider<N>,
    max_read_per_epoch: usize,
    subscriber: Option<mpsc::Sender<BlockRangeMessage>>,
    next_start_block: BlockHashAndNumber,
    websocket_connected: bool,
    processed_count: u64,
    error_count: u64,
    command_receiver: mpsc::Receiver<Command>,
    shutdown: bool,
}

impl<N: Network> Service<N> {
    pub fn new(
        provider: RootProvider<N>,
        max_read_per_epoch: usize,
    ) -> (Self, mpsc::Sender<Command>) {
        let (cmd_tx, cmd_rx) = mpsc::channel(100);

        let service = Self {
            provider,
            max_read_per_epoch,
            subscriber: None,
            next_start_block: BlockHashAndNumber::default(),
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

    async fn handle_command(&mut self, command: Command) -> Result<(), BlockRangeScannerError> {
        match command {
            Command::StreamLive { sender, block_confirmations, response } => {
                self.ensure_no_subscriber()?;
                info!("Starting live stream");
                self.subscriber = Some(sender);
                let result = self.handle_live(block_confirmations).await;
                let _ = response.send(result);
            }
            Command::StreamHistorical { sender, start_height, end_height, response } => {
                self.ensure_no_subscriber()?;
                info!(start_height = ?start_height, end_height = ?end_height, "Starting historical stream");
                self.subscriber = Some(sender);
                let result = self.handle_historical(start_height, end_height).await;
                let _ = response.send(result);
            }
            Command::StreamFrom { sender, start_height, block_confirmations, response } => {
                self.ensure_no_subscriber()?;
                self.subscriber = Some(sender);
                info!(start_height = ?start_height, "Starting streaming from");
                let result = self.handle_sync(start_height, block_confirmations).await;
                let _ = response.send(result);
            }
            Command::Unsubscribe { response } => {
                self.handle_unsubscribe();
                let _ = response.send(Ok(()));
            }
            Command::Shutdown { response } => {
                self.shutdown = true;
                self.handle_unsubscribe();
                let _ = response.send(Ok(()));
            }
        }
        Ok(())
    }

    async fn handle_live(
        &mut self,
        block_confirmations: u64,
    ) -> Result<(), BlockRangeScannerError> {
        let Some(sender) = self.subscriber.clone() else {
            return Err(BlockRangeScannerError::ServiceShutdown);
        };

        let max_read_per_epoch = self.max_read_per_epoch;
        let provider = self.provider.clone();
        let latest = self.provider.get_block_number().await?;

        // the next block returned by the underlying subscription will always be `latest + 1`,
        // because `latest` was already mined and subscription by definition only streams after new
        // blocks have been mined
        let range_start = (latest + 1).saturating_sub(block_confirmations);

        tokio::spawn(async move {
            Self::stream_live_blocks(
                range_start,
                provider,
                sender,
                block_confirmations,
                max_read_per_epoch,
            )
            .await;
        });

        Ok(())
    }

    async fn handle_historical(
        &mut self,
        start_height: BlockNumberOrTag,
        end_height: BlockNumberOrTag,
    ) -> Result<(), BlockRangeScannerError> {
        let start_block = self.provider.get_block_by_number(start_height).await?.ok_or(
            BlockRangeScannerError::HistoricalSyncError(format!(
                "Start block {start_height:?} not found"
            )),
        )?;
        let end_block = self.provider.get_block_by_number(end_height).await?.ok_or(
            BlockRangeScannerError::HistoricalSyncError(format!(
                "End block {end_height:?} not found"
            )),
        )?;

        if end_block.header().number() < start_block.header().number() {
            return Err(BlockRangeScannerError::HistoricalSyncError(format!(
                "End block {end_height:?} is lower than start block {start_height:?}"
            )));
        }

        info!(
            start_block = start_block.header().number(),
            end_block = end_block.header().number(),
            "Syncing historical data"
        );

        self.sync_historical_data(start_block, end_block).await?;

        _ = self.subscriber.take();

        info!("Successfully synced historical data, closing the stream");

        Ok(())
    }

    async fn handle_sync(
        &mut self,
        start_height: BlockNumberOrTag,
        block_confirmations: u64,
    ) -> Result<(), BlockRangeScannerError> {
        let max_read_per_epoch = self.max_read_per_epoch;
        // Step 1:
        // Fetches the starting block and end block for historical sync
        let start_block = self.provider.get_block_by_number(start_height).await?.ok_or(
            BlockRangeScannerError::HistoricalSyncError(format!(
                "Start block {start_height:?} not found"
            )),
        )?;

        let latest_block =
            self.provider.get_block_by_number(BlockNumberOrTag::Latest).await?.ok_or(
                BlockRangeScannerError::HistoricalSyncError("Latest block not found".to_string()),
            )?;

        let confirmed_tip_num = latest_block.header().number().saturating_sub(block_confirmations);

        // If start is beyond confirmed tip, skip historical and go straight to live
        if start_block.header().number() > confirmed_tip_num {
            info!(
                start_block = start_block.header().number(),
                confirmed_tip = confirmed_tip_num,
                "Start block is beyond confirmed tip, starting live stream"
            );

            let Some(sender) = self.subscriber.clone() else {
                return Err(BlockRangeScannerError::ServiceShutdown);
            };

            let provider = self.provider.clone();
            let expected_next = start_block.header().number();
            tokio::spawn(async move {
                Self::stream_live_blocks(
                    expected_next,
                    provider,
                    sender,
                    block_confirmations,
                    max_read_per_epoch,
                )
                .await;
            });

            return Ok(());
        }

        let end_block = self.provider.get_block_by_number(confirmed_tip_num.into()).await?.ok_or(
            BlockRangeScannerError::HistoricalSyncError(format!(
                "Confirmed tip block {confirmed_tip_num} not found"
            )),
        )?;

        info!(
            start_block = start_block.header().number(),
            end_block = end_block.header().number(),
            "Syncing historical data"
        );

        // Step 2: Setup the live streaming buffer
        // This channel will accumulate while historical sync is running
        let (live_block_buffer_sender, live_block_buffer_receiver) =
            mpsc::channel::<BlockRangeMessage>(MAX_BUFFERED_MESSAGES);

        let provider = self.provider.clone();

        // The cutoff is the last block we have synced historically
        // Any block > cutoff will come from the live stream
        let cutoff = end_block.header().number();

        // This task runs independently, accumulating new blocks while wehistorical data is syncing
        let live_subscription_task = tokio::spawn(async move {
            Self::stream_live_blocks(
                cutoff + 1,
                provider,
                live_block_buffer_sender,
                block_confirmations,
                max_read_per_epoch,
            )
            .await;
        });

        // Step 4: Perform historical synchronization
        // This processes blocks from start_block to end_block (cutoff)
        // If this fails, we need to abort the live streaming task
        if let Err(e) = self.sync_historical_data(start_block, end_block).await {
            warn!("aborting live_subscription_task");
            live_subscription_task.abort();
            return Err(BlockRangeScannerError::HistoricalSyncError(e.to_string()));
        }

        self.send_to_subscriber(ScannerMessage::Status(ScannerStatus::ChainTipReached)).await;

        let Some(sender) = self.subscriber.clone() else {
            return Err(BlockRangeScannerError::ServiceShutdown);
        };
        // Step 5:
        // Spawn the buffer processor task
        // This will:
        // 1. Process all buffered blocks, filtering out any ≤ cutoff
        // 2. Forward blocks > cutoff to the user
        // 3. Continue forwarding until the buffer if exhausted (waits for new blocks from live
        //    stream)
        tokio::spawn(async move {
            Self::process_live_block_buffer(live_block_buffer_receiver, sender, cutoff).await;
        });

        info!("Successfully transitioned from historical to live data");
        Ok(())
    }

    async fn sync_historical_data(
        &mut self,
        start: N::BlockResponse,
        end: N::BlockResponse,
    ) -> Result<(), BlockRangeScannerError> {
        let mut batch_count = 0;

        self.next_start_block = BlockHashAndNumber::from_header::<N>(start.header());

        // must be <= to include the edge case when start == end (i.e. return the single block
        // range)
        while self.next_start_block.number <= end.header().number() {
            let batch_end_block_number = self
                .next_start_block
                .number
                .saturating_add(self.max_read_per_epoch as u64 - 1)
                .min(end.header().number());

            self.send_to_subscriber(BlockRangeMessage::Data(
                self.next_start_block.number..=batch_end_block_number,
            ))
            .await;

            batch_count += 1;
            if batch_count % 10 == 0 {
                debug!(batch_count = batch_count, "Processed historical batches");
            }

            if batch_end_block_number == end.header().number() {
                break;
            }

            let next_start_block_number = (batch_end_block_number + 1).into();
            let next_start_block =
                match self.provider.get_block_by_number(next_start_block_number).await {
                    Ok(block) => {
                        block.expect("block number is less than 'end', so it should exist")
                    }
                    Err(e) => {
                        error!(error = %e, "Failed to get block by number");
                        let e: BlockRangeScannerError = e.into();
                        self.send_to_subscriber(BlockRangeMessage::Error(e.clone())).await;
                        return Err(e);
                    }
                };
            self.next_start_block = BlockHashAndNumber::from_header::<N>(next_start_block.header());
        }

        info!(batch_count = batch_count, "Historical sync completed");

        Ok(())
    }

    async fn stream_live_blocks<P: Provider<N>>(
        mut range_start: BlockNumber,
        provider: P,
        sender: mpsc::Sender<BlockRangeMessage>,
        block_confirmations: u64,
        max_read_per_epoch: usize,
    ) {
        match Self::get_block_subscription(&provider).await {
            Ok(ws_stream) => {
                info!("WebSocket connected for live blocks");

                // ensure we start streaming only after the expected_next_block cutoff
                let cutoff = range_start;
                let mut stream =
                    ws_stream.into_stream().skip_while(|header| header.number() < cutoff);

                while let Some(incoming_block) = stream.next().await {
                    let incoming_block_num = incoming_block.number();
                    info!(block_number = incoming_block_num, "Received block header");

                    if incoming_block_num < range_start {
                        warn!("Reorg detected: sending forked range");
                        if sender
                            .send(BlockRangeMessage::Status(ScannerStatus::ReorgDetected))
                            .await
                            .is_err()
                        {
                            warn!("Downstream channel closed, stopping live blocks task");
                            return;
                        }

                        // Calculate the confirmed block position for the incoming block
                        let incoming_confirmed =
                            incoming_block_num.saturating_sub(block_confirmations);

                        // updated expected block to updated confirmed
                        range_start = incoming_confirmed;
                    }

                    let confirmed = incoming_block_num.saturating_sub(block_confirmations);
                    if confirmed >= range_start {
                        // NOTE: Edge case when difference between range end and range start >= max
                        // reads
                        let range_end = confirmed
                            .min(range_start.saturating_add(max_read_per_epoch as u64 - 1));

                        if sender
                            .send(BlockRangeMessage::Data(range_start..=range_end))
                            .await
                            .is_err()
                        {
                            warn!("Downstream channel closed, stopping live blocks task");
                            return;
                        }

                        // Overflow can not realistically happen
                        range_start = range_end + 1;
                    }
                }
            }
            Err(e) => {
                if sender.send(BlockRangeMessage::Error(e)).await.is_err() {
                    warn!("Downstream channel closed, stopping live blocks task");
                }
            }
        }
    }

    async fn process_live_block_buffer(
        mut buffer_rx: mpsc::Receiver<BlockRangeMessage>,
        sender: mpsc::Sender<BlockRangeMessage>,
        cutoff: BlockNumber,
    ) {
        let mut processed = 0;
        let mut discarded = 0;

        // Process all buffered messages
        while let Some(data) = buffer_rx.recv().await {
            match data {
                BlockRangeMessage::Data(range) => {
                    let (start, end) = (*range.start(), *range.end());
                    if start >= cutoff {
                        if sender.send(BlockRangeMessage::Data(range)).await.is_err() {
                            warn!("Subscriber channel closed, cleaning up");
                            return;
                        }
                        processed += end - start;
                    } else if end > cutoff {
                        discarded += cutoff - start;

                        let start = cutoff;
                        if sender.send(BlockRangeMessage::Data(start..=end)).await.is_err() {
                            warn!("Subscriber channel closed, cleaning up");
                            return;
                        }
                        processed += end - start;
                    } else {
                        discarded += end - start;
                    }
                }
                _ => {
                    // Could be error or status
                    if sender.send(data).await.is_err() {
                        warn!("Subscriber channel closed, cleaning up");
                        return;
                    }
                }
            }
        }

        info!(processed = processed, discarded = discarded, "Processed buffered messages");
    }

    async fn get_block_subscription(
        provider: &impl Provider<N>,
    ) -> Result<Subscription<N::HeaderResponse>, BlockRangeScannerError> {
        let ws_stream = provider
            .subscribe_blocks()
            .await
            .map_err(|_| BlockRangeScannerError::WebSocketConnectionFailed(1))?;

        Ok(ws_stream)
    }

    async fn send_to_subscriber(&mut self, message: BlockRangeMessage) {
        if let Some(ref sender) = self.subscriber {
            if let Err(err) = sender.send(message).await {
                warn!(error = %err, "Downstream channel closed, failed sending the message to subscriber");
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

    fn ensure_no_subscriber(&self) -> Result<(), BlockRangeScannerError> {
        if self.subscriber.is_some() {
            return Err(BlockRangeScannerError::MultipleSubscribers);
        }
        Ok(())
    }
}

pub struct BlockRangeScannerClient {
    command_sender: mpsc::Sender<Command>,
}

impl BlockRangeScannerClient {
    /// Creates a new subscription client.
    ///
    /// # Arguments
    ///
    /// * `command_sender` - The sender for sending commands to the subscription service.
    #[must_use]
    pub fn new(command_sender: mpsc::Sender<Command>) -> Self {
        Self { command_sender }
    }

    /// Streams live blocks starting from the latest block.
    ///
    /// # Arguments
    ///
    /// * `block_confirmations` - Number of confirmations to apply once in live mode.
    ///
    /// # Errors
    ///
    /// * `BlockRangeScannerError::ServiceShutdown` - if the service is already shutting down.
    pub async fn stream_live(
        &self,
        block_confirmations: u64,
    ) -> Result<ReceiverStream<BlockRangeMessage>, BlockRangeScannerError> {
        let (blocks_sender, blocks_receiver) = mpsc::channel(MAX_BUFFERED_MESSAGES);
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::StreamLive {
            sender: blocks_sender,
            block_confirmations,
            response: response_tx,
        };

        self.command_sender
            .send(command)
            .await
            .map_err(|_| BlockRangeScannerError::ServiceShutdown)?;

        response_rx.await.map_err(|_| BlockRangeScannerError::ServiceShutdown)??;

        Ok(ReceiverStream::new(blocks_receiver))
    }

    /// Streams a batch of historical blocks from `start_height` to `end_height`.
    ///
    /// # Arguments
    ///
    /// * `start_height` - The starting block number or tag.
    /// * `end_height` - The ending block number or tag.
    ///
    /// # Errors
    ///
    /// * `BlockRangeScannerError::ServiceShutdown` - if the service is already shutting down.
    pub async fn stream_historical<N: Into<BlockNumberOrTag>>(
        &self,
        start_height: N,
        end_height: N,
    ) -> Result<ReceiverStream<BlockRangeMessage>, BlockRangeScannerError> {
        let (blocks_sender, blocks_receiver) = mpsc::channel(MAX_BUFFERED_MESSAGES);
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::StreamHistorical {
            sender: blocks_sender,
            start_height: start_height.into(),
            end_height: end_height.into(),
            response: response_tx,
        };

        self.command_sender
            .send(command)
            .await
            .map_err(|_| BlockRangeScannerError::ServiceShutdown)?;

        response_rx.await.map_err(|_| BlockRangeScannerError::ServiceShutdown)??;

        Ok(ReceiverStream::new(blocks_receiver))
    }

    /// Streams blocks starting from `start_height` and transitions to live mode.
    ///
    /// # Arguments
    ///
    /// * `start_height` - The starting block number or tag.
    /// * `block_confirmations` - Number of confirmations to apply once in live mode.
    ///
    /// # Errors
    ///
    /// * `BlockRangeScannerError::ServiceShutdown` - if the service is already shutting down.
    pub async fn stream_from(
        &self,
        start_height: BlockNumberOrTag,
        block_confirmations: u64,
    ) -> Result<ReceiverStream<BlockRangeMessage>, BlockRangeScannerError> {
        let (blocks_sender, blocks_receiver) = mpsc::channel(MAX_BUFFERED_MESSAGES);
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::StreamFrom {
            sender: blocks_sender,
            start_height,
            block_confirmations,
            response: response_tx,
        };

        self.command_sender
            .send(command)
            .await
            .map_err(|_| BlockRangeScannerError::ServiceShutdown)?;

        response_rx.await.map_err(|_| BlockRangeScannerError::ServiceShutdown)??;

        Ok(ReceiverStream::new(blocks_receiver))
    }

    /// Unsubscribes the current subscriber.
    ///
    /// # Errors
    ///
    /// * `BlockRangeScannerError::ServiceShutdown` - if the service is already shutting down.
    pub async fn unsubscribe(&self) -> Result<(), BlockRangeScannerError> {
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::Unsubscribe { response: response_tx };

        self.command_sender
            .send(command)
            .await
            .map_err(|_| BlockRangeScannerError::ServiceShutdown)?;

        response_rx.await.map_err(|_| BlockRangeScannerError::ServiceShutdown)?
    }

    /// Shuts down the subscription service and unsubscribes the current subscriber.
    ///
    /// # Errors
    ///
    /// * `BlockRangeScannerError::ServiceShutdown` - if the service is already shutting down.
    pub async fn shutdown(&self) -> Result<(), BlockRangeScannerError> {
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::Shutdown { response: response_tx };

        self.command_sender
            .send(command)
            .await
            .map_err(|_| BlockRangeScannerError::ServiceShutdown)?;

        response_rx.await.map_err(|_| BlockRangeScannerError::ServiceShutdown)?
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use alloy::{
        network::Ethereum,
        providers::{ProviderBuilder, ext::AnvilApi},
        rpc::{client::RpcClient, types::anvil::ReorgOptions},
        transports::mock::Asserter,
    };
    use alloy_node_bindings::Anvil;
    use tokio::{
        sync::mpsc::{self, Receiver},
        time::timeout,
    };
    use tokio_stream::StreamExt;

    use super::*;

    // Trait to enable receiver-type-agnostic range receival
    trait RangeReceiver {
        async fn next_range(&mut self) -> Option<BlockRangeMessage>;
    }

    impl RangeReceiver for ReceiverStream<BlockRangeMessage> {
        async fn next_range(&mut self) -> Option<BlockRangeMessage> {
            self.next().await
        }
    }

    impl RangeReceiver for Receiver<BlockRangeMessage> {
        async fn next_range(&mut self) -> Option<BlockRangeMessage> {
            self.recv().await
        }
    }

    macro_rules! assert_next_range {
        ($recv:expr, None) => {
            let next = $recv.next_range().await;
            assert!(next.is_none());
        };
        ($recv:expr, $range:expr) => {
            let next = $recv.next_range().await;
            if let Some(BlockRangeMessage::Data(range)) = next {
                assert_eq!($range, range);
            } else {
                panic!("expected block range, got: {next:?}");
            }
        };
    }

    fn mocked_provider(asserter: Asserter) -> RootProvider<Ethereum> {
        RootProvider::new(RpcClient::mocked(asserter))
    }

    #[test]
    fn block_range_scanner_defaults_match_constants() {
        let scanner = BlockRangeScanner::new();

        assert_eq!(scanner.max_read_per_epoch, DEFAULT_BLOCKS_READ_PER_EPOCH);
    }

    #[test]
    fn builder_methods_update_configuration() {
        let blocks_read_per_epoch = 42;

        let scanner = BlockRangeScanner::new().with_max_read_per_epoch(blocks_read_per_epoch);

        assert_eq!(scanner.max_read_per_epoch, blocks_read_per_epoch);
    }

    #[tokio::test]
    async fn send_to_subscriber_increments_processed_count() -> anyhow::Result<()> {
        let asserter = Asserter::new();
        let provider = mocked_provider(asserter);
        let (mut service, _cmd) = Service::new(provider, DEFAULT_BLOCKS_READ_PER_EPOCH);

        let (tx, mut rx) = mpsc::channel(1);
        service.subscriber = Some(tx);

        let expected_range = 10..=11;
        service.send_to_subscriber(BlockRangeMessage::Data(expected_range.clone())).await;

        assert_eq!(service.processed_count, 1);
        assert!(service.subscriber.is_some());

        let BlockRangeMessage::Data(received) = rx.recv().await.expect("range received") else {
            panic!("expected BlockRange message")
        };
        assert_eq!(received, expected_range);

        Ok(())
    }

    #[tokio::test]
    async fn send_to_subscriber_removes_closed_channel() -> anyhow::Result<()> {
        let asserter = Asserter::new();
        let provider = mocked_provider(asserter);
        let (mut service, _cmd) = Service::new(provider, DEFAULT_BLOCKS_READ_PER_EPOCH);

        let (tx, rx) = mpsc::channel(1);
        service.websocket_connected = true;
        service.subscriber = Some(tx);
        // channel is closed
        drop(rx);

        service.send_to_subscriber(BlockRangeMessage::Data(15..=15)).await;

        assert!(service.subscriber.is_none());
        assert!(!service.websocket_connected);
        assert_eq!(service.processed_count, 0);

        Ok(())
    }

    #[test]
    fn handle_unsubscribe_clears_subscriber() {
        let asserter = Asserter::new();
        let provider = mocked_provider(asserter);
        let (mut service, _cmd) = Service::new(provider, DEFAULT_BLOCKS_READ_PER_EPOCH);

        let (tx, _rx) = mpsc::channel(1);
        service.websocket_connected = true;
        service.subscriber = Some(tx);

        service.handle_unsubscribe();

        assert!(service.subscriber.is_none());
        assert!(!service.websocket_connected);
    }

    #[tokio::test]
    async fn live_mode_processes_all_blocks() -> anyhow::Result<()> {
        let anvil = Anvil::new().block_time_f64(0.01).try_spawn()?;

        let client = BlockRangeScanner::new()
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        let expected_blocks = 10;

        let mut receiver =
            client.stream_live(DEFAULT_BLOCK_CONFIRMATIONS).await?.take(expected_blocks);

        let mut block_range_start = 0;

        while let Some(BlockRangeMessage::Data(range)) = receiver.next().await {
            info!("Received block range: [{range:?}]");
            if block_range_start == 0 {
                block_range_start = *range.start();
            }

            assert_eq!(block_range_start, *range.start());
            assert!(range.end() >= range.start());
            block_range_start = *range.end() + 1;
        }

        Ok(())
    }

    #[tokio::test]
    async fn stream_from_latest_starts_at_tip_not_confirmed() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn()?;

        let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;
        provider.anvil_mine(Option::Some(20), Option::None).await?;

        let block_confirmations = 5;

        let client = BlockRangeScanner::new()
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        let expected_blocks = 10;
        let mut receiver = client
            .stream_from(BlockNumberOrTag::Latest, block_confirmations)
            .await?
            .take(expected_blocks);

        let latest_head = provider.get_block_number().await?;
        provider.anvil_mine(Option::Some(20), Option::None).await?;

        let mut expected_range_start = latest_head;

        while let Some(BlockRangeMessage::Data(range)) = receiver.next().await {
            assert_eq!(expected_range_start, *range.start());
            assert_eq!(range.end(), range.start());
            expected_range_start += 1;
        }

        // verify that the final block number (range.end) was of the latest block with the expected
        // block confirmations
        assert_eq!(expected_range_start, latest_head + expected_blocks as u64);

        Ok(())
    }

    #[tokio::test]
    async fn live_mode_respects_block_confirmations() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn()?;

        let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;
        provider.anvil_mine(Option::Some(20), Option::None).await?;

        let block_confirmations = 5;

        let client = BlockRangeScanner::new()
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        let expected_blocks = 10;

        let mut receiver = client.stream_live(block_confirmations).await?.take(expected_blocks);
        let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;
        let latest_head = provider.get_block_number().await?;
        provider.anvil_mine(Option::Some(expected_blocks as u64), Option::None).await?;

        let mut expected_range_start = latest_head.saturating_sub(block_confirmations) + 1;

        while let Some(BlockRangeMessage::Data(range)) = receiver.next().await {
            assert_eq!(expected_range_start, *range.start());
            assert_eq!(range.end(), range.start());
            expected_range_start += 1;
        }

        // we add 1 to the right side, because we're expecting the number of the _next_ block to be
        // mined
        assert_eq!(
            expected_range_start,
            latest_head + expected_blocks as u64 + 1 - block_confirmations
        );

        Ok(())
    }

    #[tokio::test]
    async fn live_mode_respects_block_confirmations_on_new_chain() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn()?;

        let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

        let block_confirmations = 5;

        let client = BlockRangeScanner::new()
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        let mut receiver = client.stream_live(block_confirmations).await?;

        provider.anvil_mine(Option::Some(6), Option::None).await?;

        let next = receiver.next().await;
        if let Some(BlockRangeMessage::Data(range)) = next {
            assert_eq!(0, *range.start());
            assert_eq!(0, *range.end());
        } else {
            panic!("expected range, got: {next:?}");
        }

        let next = receiver.next().await;
        if let Some(BlockRangeMessage::Data(range)) = next {
            assert_eq!(1, *range.start());
            assert_eq!(1, *range.end());
        } else {
            panic!("expected range, got: {next:?}");
        }

        // assert no new pending confirmed block ranges
        assert!(
            timeout(Duration::from_secs(1), async move { receiver.next().await }).await.is_err()
        );

        Ok(())
    }

    #[tokio::test]
    #[ignore = "Flaky test, see: https://github.com/OpenZeppelin/Event-Scanner/issues/109"]
    async fn continuous_blocks_if_reorg_less_than_block_confirmation() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn()?;

        let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

        let block_confirmations = 5;

        let client = BlockRangeScanner::new()
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        let mut receiver = client.stream_live(block_confirmations).await?;

        provider.anvil_mine(Option::Some(10), Option::None).await?;

        provider
            .anvil_reorg(ReorgOptions { depth: block_confirmations - 1, tx_block_pairs: vec![] })
            .await?;

        provider.anvil_mine(Option::Some(20), Option::None).await?;

        let mut block_range_start = 0;

        let end_loop = 20;
        let mut i = 0;
        while let Some(BlockRangeMessage::Data(range)) = receiver.next().await {
            if block_range_start == 0 {
                block_range_start = *range.start();
            }

            assert_eq!(block_range_start, *range.start());
            assert!(range.end() >= range.start());
            block_range_start = *range.end() + 1;
            i += 1;
            if i == end_loop {
                break;
            }
        }
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Flaky test, see: https://github.com/OpenZeppelin/Event-Scanner/issues/109"]
    async fn shallow_block_confirmation_does_not_mitigate_reorg() -> anyhow::Result<()> {
        let anvil = Anvil::new().block_time(1).try_spawn()?;

        let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

        let block_confirmations = 3;

        let client = BlockRangeScanner::new()
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        let mut receiver = client.stream_live(block_confirmations).await?;

        provider.anvil_mine(Option::Some(10), Option::None).await?;

        provider
            .anvil_reorg(ReorgOptions { depth: block_confirmations + 5, tx_block_pairs: vec![] })
            .await?;

        provider.anvil_mine(Option::Some(30), Option::None).await?;
        receiver.close();

        let mut block_range_start = 0;

        let mut block_num = vec![];
        let mut reorg_detected = false;
        while let Some(msg) = receiver.next().await {
            match msg {
                BlockRangeMessage::Data(range) => {
                    if block_range_start == 0 {
                        block_range_start = *range.start();
                    }
                    block_num.push(range);
                    if block_num.len() == 15 {
                        break;
                    }
                }
                BlockRangeMessage::Status(ScannerStatus::ReorgDetected) => {
                    reorg_detected = true;
                }
                _ => {
                    break;
                }
            }
        }
        assert!(reorg_detected, "Reorg should have been detected");

        // Generally check that there is a reorg in the range i.e.
        //                                                        REORG
        // [0..=0, 1..=1, 2..=2, 3..=3, 4..=4, 5..=5, 6..=6, 7..=7, 3..=3, 4..=4, 5..=5, 6..=6,
        // 7..=7, 8..=8, 9..=9] (Less flaky to assert this way)
        let mut found_reorg_pattern = false;
        for window in block_num.windows(2) {
            if window[1].start() < window[0].end() {
                found_reorg_pattern = true;
                break;
            }
        }
        assert!(found_reorg_pattern,);

        Ok(())
    }

    #[tokio::test]
    async fn historic_mode_respects_blocks_read_per_epoch() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn()?;

        let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

        provider.anvil_mine(Option::Some(100), Option::None).await?;

        let client = BlockRangeScanner::new()
            .with_max_read_per_epoch(5)
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        // ranges where each batch is of max blocks per epoch size
        let mut stream = client.stream_historical(0, 19).await?;
        assert_next_range!(stream, 0..=4);
        assert_next_range!(stream, 5..=9);
        assert_next_range!(stream, 10..=14);
        assert_next_range!(stream, 15..=19);
        assert_next_range!(stream, None);

        // ranges where last batch is smaller than blocks per epoch
        let mut stream = client.stream_historical(93, 99).await?;
        assert_next_range!(stream, 93..=97);
        assert_next_range!(stream, 98..=99);
        assert_next_range!(stream, None);

        // range where blocks per epoch is larger than the number of blocks in the range
        let mut stream = client.stream_historical(3, 5).await?;
        assert_next_range!(stream, 3..=5);
        assert_next_range!(stream, None);

        // single item range
        let mut stream = client.stream_historical(3, 3).await?;
        assert_next_range!(stream, 3..=3);
        assert_next_range!(stream, None);

        // range where blocks per epoch is larger than the number of blocks on chain
        let client = BlockRangeScanner::new()
            .with_max_read_per_epoch(200)
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        let mut stream = client.stream_historical(0, 20).await?;
        assert_next_range!(stream, 0..=20);
        assert_next_range!(stream, None);

        let mut stream = client.stream_historical(0, 99).await?;
        assert_next_range!(stream, 0..=99);
        assert_next_range!(stream, None);

        Ok(())
    }

    #[tokio::test]
    async fn buffered_messages_trim_ranges_prior_to_cutoff() -> anyhow::Result<()> {
        let cutoff = 50;
        let (buffer_tx, buffer_rx) = mpsc::channel(8);
        buffer_tx.send(BlockRangeMessage::Data(51..=55)).await.unwrap();
        buffer_tx.send(BlockRangeMessage::Data(56..=60)).await.unwrap();
        buffer_tx.send(BlockRangeMessage::Data(61..=70)).await.unwrap();
        drop(buffer_tx);

        let (out_tx, mut out_rx) = mpsc::channel(8);
        Service::<Ethereum>::process_live_block_buffer(buffer_rx, out_tx, cutoff).await;

        let mut forwarded = Vec::new();
        while let Some(BlockRangeMessage::Data(range)) = out_rx.recv().await {
            forwarded.push(range);
        }

        // All ranges should be forwarded as-is since they're after cutoff
        assert_eq!(forwarded, vec![51..=55, 56..=60, 61..=70]);
        Ok(())
    }

    #[tokio::test]
    async fn ranges_entirely_before_cutoff_are_discarded() -> anyhow::Result<()> {
        let cutoff = 100;

        let (buffer_tx, buffer_rx) = mpsc::channel(8);
        buffer_tx.send(BlockRangeMessage::Data(40..=50)).await.unwrap();
        buffer_tx.send(BlockRangeMessage::Data(51..=60)).await.unwrap();
        buffer_tx.send(BlockRangeMessage::Data(61..=70)).await.unwrap();
        drop(buffer_tx);

        let (out_tx, mut out_rx) = mpsc::channel(8);
        Service::<Ethereum>::process_live_block_buffer(buffer_rx, out_tx, cutoff).await;

        let mut forwarded = Vec::new();
        while let Some(BlockRangeMessage::Data(range)) = out_rx.recv().await {
            forwarded.push(range);
        }

        // All ranges should be discarded since they're before cutoff
        assert_eq!(forwarded, vec![]);
        Ok(())
    }

    #[tokio::test]
    async fn ranges_overlapping_cutoff_are_trimmed() -> anyhow::Result<()> {
        let cutoff = 75;

        let (buffer_tx, buffer_rx) = mpsc::channel(8);
        buffer_tx.send(BlockRangeMessage::Data(70..=80)).await.unwrap();
        buffer_tx.send(BlockRangeMessage::Data(60..=80)).await.unwrap();
        buffer_tx.send(BlockRangeMessage::Data(74..=76)).await.unwrap();
        drop(buffer_tx);

        let (out_tx, mut out_rx) = mpsc::channel(8);
        Service::<Ethereum>::process_live_block_buffer(buffer_rx, out_tx, cutoff).await;

        let mut forwarded = Vec::new();
        while let Some(BlockRangeMessage::Data(range)) = out_rx.recv().await {
            forwarded.push(range);
        }

        // All ranges should be trimmed to start at cutoff (75)
        assert_eq!(forwarded, vec![75..=80, 75..=80, 75..=76]);
        Ok(())
    }

    #[tokio::test]
    async fn mixed_ranges_are_handled_correctly() -> anyhow::Result<()> {
        let cutoff = 50;

        let (buffer_tx, buffer_rx) = mpsc::channel(8);
        buffer_tx.send(BlockRangeMessage::Data(30..=45)).await.unwrap(); // Before cutoff: discard
        buffer_tx.send(BlockRangeMessage::Data(46..=55)).await.unwrap(); // Overlaps: trim to 50..=55
        buffer_tx.send(BlockRangeMessage::Data(56..=65)).await.unwrap(); // After cutoff: forward as-is
        buffer_tx.send(BlockRangeMessage::Data(40..=49)).await.unwrap(); // Before cutoff: discard
        buffer_tx.send(BlockRangeMessage::Data(49..=51)).await.unwrap(); // Overlaps: trim to 50..=51
        buffer_tx.send(BlockRangeMessage::Data(51..=100)).await.unwrap(); // After cutoff: forward as-is
        drop(buffer_tx);

        let (out_tx, mut out_rx) = mpsc::channel(8);
        Service::<Ethereum>::process_live_block_buffer(buffer_rx, out_tx, cutoff).await;

        let mut forwarded = Vec::new();
        while let Some(BlockRangeMessage::Data(range)) = out_rx.recv().await {
            forwarded.push(range);
        }

        assert_eq!(forwarded, vec![50..=55, 56..=65, 50..=51, 51..=100]);
        Ok(())
    }

    #[tokio::test]
    async fn edge_case_range_exactly_at_cutoff() -> anyhow::Result<()> {
        let cutoff = 100;

        let (buffer_tx, buffer_rx) = mpsc::channel(8);
        buffer_tx.send(BlockRangeMessage::Data(99..=99)).await.unwrap(); // Just before: discard
        buffer_tx.send(BlockRangeMessage::Data(100..=100)).await.unwrap(); // Exactly at: forward
        buffer_tx.send(BlockRangeMessage::Data(99..=100)).await.unwrap(); // Includes cutoff: trim to 100..=100
        buffer_tx.send(BlockRangeMessage::Data(100..=101)).await.unwrap(); // Starts at cutoff: forward
        drop(buffer_tx);

        let (out_tx, mut out_rx) = mpsc::channel(8);
        Service::<Ethereum>::process_live_block_buffer(buffer_rx, out_tx, cutoff).await;

        let mut forwarded = Vec::new();
        while let Some(BlockRangeMessage::Data(range)) = out_rx.recv().await {
            forwarded.push(range);
        }

        // ensure no duplicates
        assert_eq!(forwarded, vec![100..=100, 100..=101]);
        Ok(())
    }

    #[tokio::test]
    async fn cutoff_at_zero_handles_all_ranges() -> anyhow::Result<()> {
        let cutoff = 0;

        let (buffer_tx, buffer_rx) = mpsc::channel(8);
        buffer_tx.send(BlockRangeMessage::Data(0..=5)).await.unwrap();
        buffer_tx.send(BlockRangeMessage::Data(6..=10)).await.unwrap();
        buffer_tx.send(BlockRangeMessage::Data(11..=25)).await.unwrap();
        drop(buffer_tx);

        let (out_tx, mut out_rx) = mpsc::channel(8);
        Service::<Ethereum>::process_live_block_buffer(buffer_rx, out_tx, cutoff).await;

        let mut forwarded = Vec::new();
        while let Some(BlockRangeMessage::Data(range)) = out_rx.recv().await {
            forwarded.push(range);
        }

        // All ranges should be forwarded since they're all >= 0
        assert_eq!(forwarded, vec![0..=5, 6..=10, 11..=25]);
        Ok(())
    }

    #[tokio::test]
    async fn forwards_errors_to_subscribers() -> anyhow::Result<()> {
        let asserter = Asserter::new();
        let provider = mocked_provider(asserter);
        let (mut service, _cmd) = Service::new(provider, DEFAULT_BLOCKS_READ_PER_EPOCH);

        let (tx, mut rx) = mpsc::channel(1);
        service.subscriber = Some(tx);

        service
            .send_to_subscriber(BlockRangeMessage::Error(
                BlockRangeScannerError::WebSocketConnectionFailed(4),
            ))
            .await;

        match rx.recv().await.expect("subscriber should stay open") {
            BlockRangeMessage::Error(BlockRangeScannerError::WebSocketConnectionFailed(
                attempts,
            )) => {
                assert_eq!(attempts, 4);
            }
            other => panic!("unexpected message: {other:?}"),
        }

        Ok(())
    }
}
