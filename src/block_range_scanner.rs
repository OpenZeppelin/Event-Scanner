//! Example usage:
//!
//! ```rust,no_run
//! use alloy::{eips::BlockNumberOrTag, network::Ethereum, primitives::BlockNumber};
//! use std::ops::RangeInclusive;
//! use tokio_stream::{StreamExt, wrappers::ReceiverStream};
//!
//! use alloy::transports::http::reqwest::Url;
//! use event_scanner::block_range_scanner::{
//!     BlockRangeScanner, BlockRangeScannerClient, Error as BlockRangeScannerError,
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
//!         .with_blocks_read_per_epoch(1000)
//!         .with_reorg_rewind_depth(5)
//!         .with_block_confirmations(5)
//!         .connect_ws::<Ethereum>(Url::parse("ws://localhost:8546").unwrap())
//!         .await?;
//!
//!     // Create client to send subscribe command to block scanner
//!     let client: BlockRangeScannerClient = block_range_scanner.run()?;
//!
//!     let mut receiver = client
//!         .subscribe(
//!             BlockNumberOrTag::Latest,
//!             None, // just subscribe to new blocks
//!         )
//!         .await?;
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
//!                     BlockRangeScannerError::ServiceShutdown => break,
//!                     BlockRangeScannerError::WebSocketConnectionFailed(_) => {
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

use std::ops::RangeInclusive;

use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;

use alloy::{
    consensus::BlockHeader,
    eips::BlockNumberOrTag,
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
const DEFAULT_BLOCK_CONFIRMATIONS: u64 = 0;
// const BACK_OFF_MAX_RETRIES: u64 = 5;

const MAX_BUFFERED_MESSAGES: usize = 50000;

const DEFAULT_REORG_REWIND_DEPTH: u64 = 0;

// // State sync aware retry settings
// const STATE_SYNC_RETRY_INTERVAL: Duration = Duration::from_secs(30);
// const STATE_SYNC_MAX_RETRIES: u64 = 12;

#[derive(Error, Debug)]
pub enum Error {
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
        sender: mpsc::Sender<Result<RangeInclusive<BlockNumber>, Error>>,
        start_height: BlockNumberOrTag,
        end_height: Option<BlockNumberOrTag>,
        response: oneshot::Sender<Result<(), Error>>,
    },
    Unsubscribe {
        response: oneshot::Sender<Result<(), Error>>,
    },
    GetStatus {
        response: oneshot::Sender<ServiceStatus>,
    },
    Shutdown {
        response: oneshot::Sender<Result<(), Error>>,
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
    pub hash: BlockHash,
    pub number: BlockNumber,
}

impl BlockHashAndNumber {
    fn from_header<N: Network>(header: &N::HeaderResponse) -> Self {
        Self { hash: header.hash(), number: header.number() }
    }
}

#[derive(Clone)]
struct Config {
    blocks_read_per_epoch: usize,
    reorg_rewind_depth: u64,
    #[allow(
        dead_code,
        reason = "Will be used in reorg mechanism: https://github.com/OpenZeppelin/Event-Scanner/issues/5"
    )]
    block_confirmations: u64,
}

pub struct BlockRangeScanner {
    blocks_read_per_epoch: usize,
    reorg_rewind_depth: u64,
    block_confirmations: u64,
}

impl Default for BlockRangeScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockRangeScanner {
    #[must_use]
    pub fn new() -> Self {
        Self {
            blocks_read_per_epoch: DEFAULT_BLOCKS_READ_PER_EPOCH,
            reorg_rewind_depth: DEFAULT_REORG_REWIND_DEPTH,
            block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS,
        }
    }

    #[must_use]
    pub fn with_blocks_read_per_epoch(mut self, blocks_read_per_epoch: usize) -> Self {
        self.blocks_read_per_epoch = blocks_read_per_epoch;
        self
    }

    #[must_use]
    pub fn with_reorg_rewind_depth(mut self, reorg_rewind_depth: u64) -> Self {
        self.reorg_rewind_depth = reorg_rewind_depth;
        self
    }

    #[must_use]
    pub fn with_block_confirmations(mut self, block_confirmations: u64) -> Self {
        self.block_confirmations = block_confirmations;
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
        Ok(ConnectedBlockRangeScanner {
            provider,
            config: Config {
                blocks_read_per_epoch: self.blocks_read_per_epoch,
                reorg_rewind_depth: self.reorg_rewind_depth,
                block_confirmations: self.block_confirmations,
            },
        })
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
        Ok(ConnectedBlockRangeScanner {
            provider,
            config: Config {
                blocks_read_per_epoch: self.blocks_read_per_epoch,
                reorg_rewind_depth: self.reorg_rewind_depth,
                block_confirmations: self.block_confirmations,
            },
        })
    }
}

pub struct ConnectedBlockRangeScanner<N: Network> {
    provider: RootProvider<N>,
    config: Config,
}

impl<N: Network> ConnectedBlockRangeScanner<N> {
    /// Returns the underlying Provider.
    #[must_use]
    pub fn provider(&self) -> &impl Provider<N> {
        &self.provider
    }

    /// Starts the subscription service and returns a client for sending commands.
    ///
    /// # Errors
    ///
    /// Returns an error if the subscription service fails to start.
    pub fn run(&self) -> anyhow::Result<BlockRangeScannerClient> {
        let (service, cmd_tx) = Service::new(self.config.clone(), self.provider.clone());
        tokio::spawn(async move {
            service.run().await;
        });
        Ok(BlockRangeScannerClient::new(cmd_tx))
    }
}

struct Service<N: Network> {
    config: Config,
    provider: RootProvider<N>,
    subscriber: Option<mpsc::Sender<Result<RangeInclusive<BlockNumber>, Error>>>,
    current: Option<BlockHashAndNumber>,
    websocket_connected: bool,
    processed_count: u64,
    error_count: u64,
    command_receiver: mpsc::Receiver<Command>,
    shutdown: bool,
}

impl<N: Network> Service<N> {
    pub fn new(config: Config, provider: RootProvider<N>) -> (Self, mpsc::Sender<Command>) {
        let (cmd_tx, cmd_rx) = mpsc::channel(100);

        let service = Self {
            config,
            provider,
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

    async fn handle_command(&mut self, command: Command) -> Result<(), Error> {
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
        sender: mpsc::Sender<Result<RangeInclusive<BlockNumber>, Error>>,
        start_height: BlockNumberOrTag,
        end_height: Option<BlockNumberOrTag>,
    ) -> Result<(), Error> {
        if self.subscriber.is_some() {
            return Err(Error::MultipleSubscribers);
        }

        info!(start_height = ?start_height, "Starting subscription from point");
        self.subscriber = Some(sender);

        if let Some(end_height) = end_height {
            self.sync_historical(start_height, end_height).await?;
            return Ok(());
        }

        if matches!(start_height, BlockNumberOrTag::Latest) {
            self.sync_live().await?;
        } else {
            self.sync_historical_and_transition_to_live(start_height).await?;
        }

        Ok(())
    }

    async fn sync_historical(
        &mut self,
        start_height: BlockNumberOrTag,
        end_height: BlockNumberOrTag,
    ) -> Result<(), Error> {
        let start_block =
            self.provider.get_block_by_number(start_height).await?.expect("block does not exist");
        let end_block =
            self.provider.get_block_by_number(end_height).await?.expect("block does not exist");

        info!(
            start_block = start_block.header().number(),
            end_block = end_block.header().number(),
            "Syncing historical data"
        );

        self.sync_historical_data(start_block, end_block).await?;

        info!("Successfully synced historical data");

        if let Some(sender) = &self.subscriber
            && sender.send(Err(Error::Eof)).await.is_err()
        {
            warn!("Subscriber channel closed, cleaning up");
        }

        Ok(())
    }

    async fn sync_historical_and_transition_to_live(
        &mut self,
        start_height: BlockNumberOrTag,
    ) -> Result<(), Error> {
        // Step 1:
        // Fetches the starting block and end block for historical sync
        let start_block =
            self.provider.get_block_by_number(start_height).await?.expect("already checked");

        let end_block = self
            .provider
            .get_block_by_number(BlockNumberOrTag::Latest)
            .await?
            .expect("should be valid");

        info!(
            start_block = start_block.header().number(),
            end_block = end_block.header().number(),
            "Syncing historical data"
        );

        // Step 2: Setup the live streaming buffer
        // This channel will accumulate while historical sync is running
        let (live_block_buffer_sender, live_block_buffer_receiver) =
            mpsc::channel::<Result<RangeInclusive<BlockNumber>, Error>>(MAX_BUFFERED_MESSAGES);

        let provider = self.provider.clone();
        let rewind = self.config.reorg_rewind_depth;

        // The cutoff is the last block we have synced historically
        // Any block > cutoff will come from the live stream
        let cutoff = end_block.header().number();

        // This task runs independently, accumulating new blocks while wehistorical data is syncing
        let live_subscription_task = tokio::spawn(async move {
            Self::stream_live_blocks(cutoff + 1, provider, live_block_buffer_sender, rewind).await;
        });

        // Step 4: Perform historical synchronization
        // This processes blocks from start_block to end_block (cutoff)
        // If this fails, we need to abort the live streaming task
        if let Err(e) = self.sync_historical_data(start_block, end_block).await {
            warn!("aborting live_subscription_task");
            live_subscription_task.abort();
            return Err(Error::HistoricalSyncError(e.to_string()));
        }

        let sender = self.subscriber.clone().expect("subscriber should be set");

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

    async fn sync_live(&mut self) -> Result<(), Error> {
        let provider = self.provider.clone();
        let start = self.provider.get_block_number().await?;

        let sender = self.subscriber.clone().expect("subscriber should be set");
        let rewind = self.config.reorg_rewind_depth;
        tokio::spawn(async move {
            Self::stream_live_blocks(start, provider, sender, rewind).await;
        });

        Ok(())
    }

    async fn sync_historical_data(
        &mut self,
        start: N::BlockResponse,
        end: N::BlockResponse,
    ) -> Result<(), Error> {
        let mut batch_count = 0;

        self.current = Some(BlockHashAndNumber::from_header::<N>(start.header()));

        while self.current.as_ref().unwrap().number < end.header().number() {
            self.ensure_current_not_reorged().await?;

            let batch_to = self
                .current
                .as_ref()
                .unwrap()
                .number
                .saturating_add(self.config.blocks_read_per_epoch as u64)
                .min(end.header().number());

            let batch_end_block =
                self.provider.get_block_by_number(batch_to.into()).await?.expect("should be valid");

            self.send_to_subscriber(Ok(self.current.as_ref().unwrap().number..=batch_to)).await;

            self.current = Some(BlockHashAndNumber::from_header::<N>(batch_end_block.header()));

            batch_count += 1;
            if batch_count % 10 == 0 {
                debug!(batch_count = batch_count, "Processed historical batches");
            }
        }

        info!(batch_count = batch_count, "Historical sync completed");
        Ok(())
    }

    async fn ensure_current_not_reorged(&mut self) -> Result<(), Error> {
        let current_block =
            self.provider.get_block_by_hash(self.current.as_ref().unwrap().hash).await?;
        if current_block.is_some() {
            return Ok(());
        }

        self.rewind_on_reorg_detected().await
    }

    async fn rewind_on_reorg_detected(&mut self) -> Result<(), Error> {
        let mut new_current_height =
            if self.current.as_ref().unwrap().number <= self.config.reorg_rewind_depth {
                0
            } else {
                self.current.as_ref().unwrap().number - self.config.reorg_rewind_depth
            };

        let head = self.provider.get_block_number().await?;
        if head < new_current_height {
            new_current_height = head;
        }

        let current = self
            .provider
            .get_block_by_number(new_current_height.into())
            .await?
            .map(|block| BlockHashAndNumber::from_header::<N>(block.header()))
            .expect("block should exist");

        info!(
            old_current = self.current.as_ref().unwrap().number,
            new_current = current.number,
            "Rewind on reorg detected"
        );

        self.current = Some(current);

        Ok(())
    }

    async fn stream_live_blocks<P: Provider<N>>(
        mut current: BlockNumber,
        provider: P,
        sender: mpsc::Sender<Result<RangeInclusive<BlockNumber>, Error>>,
        reorg_rewind_depth: u64,
    ) {
        // Track previous block hash for reorg detection
        let mut prev_hash: Option<BlockHash> = None;
        if current > 0
            && let Ok(Some(prev_block)) = provider
                .get_block_by_number(BlockNumberOrTag::Number(current.saturating_sub(1)))
                .await
        {
            prev_hash = Some(prev_block.header().hash());
        }

        match Self::get_block_subscription(&provider).await {
            Ok(mut ws_stream) => {
                info!("WebSocket connected for live blocks");

                while let Ok(header_resp) = ws_stream.recv().await {
                    let num = header_resp.number();
                    info!(block_number = num, "Received block header");
                    if num < current {
                        continue;
                    }

                    // Reorg detection when the incoming header is exactly the next expected block
                    if num == current
                        && prev_hash.is_some()
                        && header_resp.parent_hash() != prev_hash.unwrap()
                    {
                        let rewind_start = current.saturating_sub(reorg_rewind_depth);
                        info!(current, rewind_start, "Reorg detected: sending rewind range");
                        if sender.send(Ok(rewind_start..=current)).await.is_err() {
                            warn!("Downstream channel closed, stopping live blocks task (reorg)");
                            return;
                        }
                        prev_hash = Some(header_resp.hash());
                        current = num + 1;
                        continue;
                    }

                    if sender.send(Ok(current..=num)).await.is_err() {
                        warn!("Downstream channel closed, stopping live blocks task");
                        return;
                    }

                    prev_hash = Some(header_resp.hash());
                    current = num + 1;
                }
            }
            Err(e) => {
                let _ = sender.send(Err(e)).await;
            }
        }
    }

    async fn process_live_block_buffer(
        mut buffer_rx: mpsc::Receiver<Result<RangeInclusive<BlockNumber>, Error>>,
        sender: mpsc::Sender<Result<RangeInclusive<BlockNumber>, Error>>,
        cutoff: BlockNumber,
    ) {
        let mut processed = 0;
        let mut discarded = 0;

        // Process all buffered messages
        while let Some(item) = buffer_rx.recv().await {
            match item {
                Ok(range) => {
                    let (start, end) = (*range.start(), *range.end());
                    if start >= cutoff {
                        if sender.send(Ok(range)).await.is_err() {
                            warn!("Subscriber channel closed, cleaning up");
                            return;
                        }
                        processed += end - start;
                    } else if end > cutoff {
                        discarded += cutoff - start;

                        let start = cutoff;
                        if sender.send(Ok(start..=end)).await.is_err() {
                            warn!("Subscriber channel closed, cleaning up");
                            return;
                        }
                        processed += end - start;
                    } else {
                        discarded += end - start;
                    }
                }
                Err(e) => {
                    warn!("Buffered live stream error: {e}");
                }
            }
        }

        info!(processed = processed, discarded = discarded, "Processed buffered messages");
    }

    async fn get_block_subscription(
        provider: &impl Provider<N>,
    ) -> Result<Subscription<N::HeaderResponse>, Error> {
        let ws_stream =
            provider.subscribe_blocks().await.map_err(|_| Error::WebSocketConnectionFailed(1))?;

        Ok(ws_stream)
    }

    async fn send_to_subscriber(&mut self, result: Result<RangeInclusive<BlockNumber>, Error>) {
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

    /// Subscribes to new blocks.
    ///
    /// # Arguments
    ///
    /// * `start_height` - The block number to start from.
    /// * `end_height` - The block number to end at (inclusive).
    ///
    /// # Errors
    ///
    /// * `Error::ServiceShutdown` - if the service is already shutting down.
    pub async fn subscribe(
        &self,
        start_height: BlockNumberOrTag,
        end_height: Option<BlockNumberOrTag>,
    ) -> Result<ReceiverStream<Result<RangeInclusive<BlockNumber>, Error>>, Error> {
        let (blocks_sender, blocks_receiver) = mpsc::channel(MAX_BUFFERED_MESSAGES);
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::Subscribe {
            sender: blocks_sender,
            start_height,
            end_height,
            response: response_tx,
        };

        self.command_sender.send(command).await.map_err(|_| Error::ServiceShutdown)?;

        response_rx.await.map_err(|_| Error::ServiceShutdown)??;

        let stream = ReceiverStream::new(blocks_receiver);

        Ok(stream)
    }

    /// Unsubscribes the current subscriber.
    ///
    /// # Errors
    ///
    /// * `Error::ServiceShutdown` - if the service is already shutting down.
    pub async fn unsubscribe(&self) -> Result<(), Error> {
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::Unsubscribe { response: response_tx };

        self.command_sender.send(command).await.map_err(|_| Error::ServiceShutdown)?;

        response_rx.await.map_err(|_| Error::ServiceShutdown)?
    }

    /// Returns the current status of the subscription service.
    ///
    /// # Errors
    ///
    /// * `Error::ServiceShutdown` - if the service is already shutting down.
    pub async fn get_status(&self) -> Result<ServiceStatus, Error> {
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::GetStatus { response: response_tx };

        self.command_sender.send(command).await.map_err(|_| Error::ServiceShutdown)?;

        response_rx.await.map_err(|_| Error::ServiceShutdown)
    }

    /// Shuts down the subscription service and unsubscribes the current subscriber.
    ///
    /// # Errors
    ///
    /// * `Error::ServiceShutdown` - if the service is already shutting down.
    pub async fn shutdown(&self) -> Result<(), Error> {
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::Shutdown { response: response_tx };

        self.command_sender.send(command).await.map_err(|_| Error::ServiceShutdown)?;

        response_rx.await.map_err(|_| Error::ServiceShutdown)?
    }
}

#[cfg(test)]
mod tests {
    use alloy::{
        eips::BlockNumberOrTag,
        network::Ethereum,
        primitives::{B256, keccak256},
        rpc::{
            client::RpcClient,
            types::{Block as RpcBlock, Header, Transaction},
        },
        transports::mock::Asserter,
    };
    use alloy_node_bindings::Anvil;
    use serde_json::{Value, json};
    use tokio::sync::mpsc;
    use tokio_stream::StreamExt;

    use super::*;

    fn test_config() -> Config {
        Config { blocks_read_per_epoch: 5, reorg_rewind_depth: 5, block_confirmations: 0 }
    }

    fn mocked_provider(asserter: Asserter) -> RootProvider<Ethereum> {
        RootProvider::new(RpcClient::mocked(asserter))
    }

    fn mock_block(number: u64, hash: B256) -> RpcBlock<Transaction, Header> {
        let mut block: RpcBlock<Transaction, Header> = RpcBlock::default();
        block.header.hash = hash;
        block.header.number = number;
        block
    }

    #[test]
    fn block_range_scanner_defaults_match_constants() {
        let scanner = BlockRangeScanner::new();

        assert_eq!(scanner.blocks_read_per_epoch, DEFAULT_BLOCKS_READ_PER_EPOCH);
        assert_eq!(scanner.reorg_rewind_depth, DEFAULT_REORG_REWIND_DEPTH);
        assert_eq!(scanner.block_confirmations, DEFAULT_BLOCK_CONFIRMATIONS);
    }

    #[test]
    fn builder_methods_update_configuration() {
        let blocks_read_per_epoch = 42;
        let reorg_rewind_depth = 12;
        let block_confirmations = 7;

        let scanner = BlockRangeScanner::new()
            .with_blocks_read_per_epoch(blocks_read_per_epoch)
            .with_reorg_rewind_depth(reorg_rewind_depth)
            .with_block_confirmations(block_confirmations);

        assert_eq!(scanner.blocks_read_per_epoch, blocks_read_per_epoch);
        assert_eq!(scanner.block_confirmations, block_confirmations);
    }

    #[test]
    fn service_status_reflects_internal_state() {
        let asserter = Asserter::new();
        let provider = mocked_provider(asserter);
        let (mut service, _cmd) = Service::new(test_config(), provider);

        let processed_count = 7;
        let error_count = 2;
        service.processed_count = processed_count;
        service.error_count = error_count;
        let hash = keccak256(b"random");
        let block_number = 99;
        service.current = Some(BlockHashAndNumber { hash, number: block_number });
        service.websocket_connected = true;
        service.subscriber = Some(mpsc::channel(1).0);

        let status = service.get_status();

        assert!(status.is_subscribed);
        assert!(status.websocket_connected);
        assert_eq!(status.processed_count, processed_count);
        assert_eq!(status.error_count, error_count);
        let last = status.last_synced_block.expect("last synced block is set");
        assert_eq!(last.number, block_number);
        assert_eq!(last.hash, hash);
    }

    #[tokio::test]
    async fn send_to_subscriber_increments_processed_count() -> anyhow::Result<()> {
        let asserter = Asserter::new();
        let provider = mocked_provider(asserter);
        let (mut service, _cmd) = Service::new(test_config(), provider);

        let (tx, mut rx) = mpsc::channel(1);
        service.subscriber = Some(tx);

        let expected_range = 10..=11;
        service.send_to_subscriber(Ok(expected_range.clone())).await;

        assert_eq!(service.processed_count, 1);
        assert!(service.subscriber.is_some());

        let received = rx.recv().await.expect("range received")?;
        assert_eq!(received, expected_range);

        Ok(())
    }

    #[tokio::test]
    async fn send_to_subscriber_removes_closed_channel() -> anyhow::Result<()> {
        let asserter = Asserter::new();
        let provider = mocked_provider(asserter);
        let (mut service, _cmd) = Service::new(test_config(), provider);

        let (tx, rx) = mpsc::channel(1);
        service.websocket_connected = true;
        service.subscriber = Some(tx);
        // channel is closed
        drop(rx);

        service.send_to_subscriber(Ok(15..=15)).await;

        assert!(service.subscriber.is_none());
        assert!(!service.websocket_connected);
        assert_eq!(service.processed_count, 0);

        Ok(())
    }

    #[test]
    fn handle_unsubscribe_clears_subscriber() {
        let asserter = Asserter::new();
        let provider = mocked_provider(asserter);
        let (mut service, _cmd) = Service::new(test_config(), provider);

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
            .with_blocks_read_per_epoch(3)
            .with_reorg_rewind_depth(5)
            .with_block_confirmations(1)
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        let expected_blocks = 10;

        let mut receiver =
            client.subscribe(BlockNumberOrTag::Latest, None).await?.take(expected_blocks);

        let mut block_range_start = 0;

        while let Some(result) = receiver.next().await {
            match result {
                Ok(range) => {
                    println!("Received block range: [{range:?}]");
                    if block_range_start == 0 {
                        block_range_start = *range.start();
                    }

                    assert_eq!(block_range_start, *range.start());
                    assert!(*range.end() >= *range.start());
                    block_range_start = *range.end() + 1;
                }
                Err(e) => {
                    panic!("Received error from subscription: {e}");
                }
            }
        }

        Ok(())
    }

    #[tokio::test]
    async fn rewinds_on_detected_reorg() -> anyhow::Result<()> {
        let asserter = Asserter::new();
        let provider = mocked_provider(asserter.clone());

        let mut config = test_config();
        config.reorg_rewind_depth = 6;
        let (mut service, _cmd) = Service::new(config.clone(), provider);

        let original_height = 10;
        let original_hash = keccak256(b"original block");
        let original_block = mock_block(original_height, original_hash);
        service.current =
            Some(BlockHashAndNumber::from_header::<Ethereum>(original_block.header()));

        let expected_rewind_height = original_height - config.reorg_rewind_depth;
        let expected_rewind_hash = keccak256(b"rewound block");
        let rewound_block = mock_block(expected_rewind_height, expected_rewind_hash);

        // Mock provider responses for reorg detection and rewind:
        // 1. get_block_by_hash(original_hash) -> None (block not found = reorg detected)
        asserter.push_success(&Value::Null);
        // 2. get_block_number() -> 12 (current chain head is at 12)
        asserter.push_success(&json!(format!("0x{:x}", original_height + 2)));
        // 3. get_block_by_number(expected_rewind_height) -> rewound_block
        asserter.push_success(&rewound_block);

        service.ensure_current_not_reorged().await?;

        let current = service.current.expect("current block should be set after rewind");
        assert_eq!(current.number, expected_rewind_height, "should rewind by reorg_rewind_depth");
        assert_eq!(current.hash, expected_rewind_hash, "should use hash of block at rewind height");

        Ok(())
    }

    #[tokio::test]
    async fn ranges_entirely_after_cutoff_are_forwarded_unchanged() -> anyhow::Result<()> {
        let cutoff = 50;

        let (buffer_tx, buffer_rx) = mpsc::channel(8);
        buffer_tx.send(Ok(51..=55)).await.unwrap();
        buffer_tx.send(Ok(60..=65)).await.unwrap();
        buffer_tx.send(Ok(70..=71)).await.unwrap();
        drop(buffer_tx);

        let (out_tx, mut out_rx) = mpsc::channel(8);
        Service::<Ethereum>::process_live_block_buffer(buffer_rx, out_tx, cutoff).await;

        let mut forwarded = Vec::new();
        while let Some(result) = out_rx.recv().await {
            forwarded.push(result.unwrap());
        }

        // All ranges should be forwarded as-is since they're after cutoff
        assert_eq!(forwarded, vec![51..=55, 60..=65, 70..=71]);
        Ok(())
    }

    #[tokio::test]
    async fn ranges_entirely_before_cutoff_are_discarded() -> anyhow::Result<()> {
        let cutoff = 100;

        let (buffer_tx, buffer_rx) = mpsc::channel(8);
        buffer_tx.send(Ok(40..=50)).await.unwrap();
        buffer_tx.send(Ok(60..=75)).await.unwrap();
        buffer_tx.send(Ok(90..=99)).await.unwrap();
        drop(buffer_tx);

        let (out_tx, mut out_rx) = mpsc::channel(8);
        Service::<Ethereum>::process_live_block_buffer(buffer_rx, out_tx, cutoff).await;

        let mut forwarded = Vec::new();
        while let Some(result) = out_rx.recv().await {
            forwarded.push(result.unwrap());
        }

        // All ranges should be discarded since they're before cutoff
        assert_eq!(forwarded, vec![]);
        Ok(())
    }

    #[tokio::test]
    async fn ranges_overlapping_cutoff_are_trimmed() -> anyhow::Result<()> {
        let cutoff = 75;

        let (buffer_tx, buffer_rx) = mpsc::channel(8);
        buffer_tx.send(Ok(70..=80)).await.unwrap();
        buffer_tx.send(Ok(60..=90)).await.unwrap();
        buffer_tx.send(Ok(74..=76)).await.unwrap();
        drop(buffer_tx);

        let (out_tx, mut out_rx) = mpsc::channel(8);
        Service::<Ethereum>::process_live_block_buffer(buffer_rx, out_tx, cutoff).await;

        let mut forwarded = Vec::new();
        while let Some(result) = out_rx.recv().await {
            forwarded.push(result.unwrap());
        }

        // All ranges should be trimmed to start at cutoff (75)
        assert_eq!(forwarded, vec![75..=80, 75..=90, 75..=76]);
        Ok(())
    }

    #[tokio::test]
    async fn mixed_ranges_are_handled_correctly() -> anyhow::Result<()> {
        let cutoff = 50;

        let (buffer_tx, buffer_rx) = mpsc::channel(8);
        buffer_tx.send(Ok(30..=40)).await.unwrap(); // Before cutoff: discard
        buffer_tx.send(Ok(45..=55)).await.unwrap(); // Overlaps: trim to 50..=55
        buffer_tx.send(Ok(60..=65)).await.unwrap(); // After cutoff: forward as-is
        buffer_tx.send(Ok(48..=49)).await.unwrap(); // Before cutoff: discard
        buffer_tx.send(Ok(49..=51)).await.unwrap(); // Overlaps: trim to 50..=51
        buffer_tx.send(Ok(100..=110)).await.unwrap(); // After cutoff: forward as-is
        drop(buffer_tx);

        let (out_tx, mut out_rx) = mpsc::channel(8);
        Service::<Ethereum>::process_live_block_buffer(buffer_rx, out_tx, cutoff).await;

        let mut forwarded = Vec::new();
        while let Some(result) = out_rx.recv().await {
            forwarded.push(result.unwrap());
        }

        assert_eq!(forwarded, vec![50..=55, 60..=65, 50..=51, 100..=110]);
        Ok(())
    }

    #[tokio::test]
    async fn edge_case_range_exactly_at_cutoff() -> anyhow::Result<()> {
        let cutoff = 100;

        let (buffer_tx, buffer_rx) = mpsc::channel(8);
        buffer_tx.send(Ok(99..=99)).await.unwrap(); // Just before: discard
        buffer_tx.send(Ok(100..=100)).await.unwrap(); // Exactly at: forward
        buffer_tx.send(Ok(99..=100)).await.unwrap(); // Includes cutoff: trim to 100..=100
        buffer_tx.send(Ok(100..=101)).await.unwrap(); // Starts at cutoff: forward
        drop(buffer_tx);

        let (out_tx, mut out_rx) = mpsc::channel(8);
        Service::<Ethereum>::process_live_block_buffer(buffer_rx, out_tx, cutoff).await;

        let mut forwarded = Vec::new();
        while let Some(result) = out_rx.recv().await {
            forwarded.push(result.unwrap());
        }

        // ensure no duplicates
        assert_eq!(forwarded, vec![100..=100, 100..=101]);
        Ok(())
    }

    #[tokio::test]
    async fn cutoff_at_zero_handles_all_ranges() -> anyhow::Result<()> {
        let cutoff = 0;

        let (buffer_tx, buffer_rx) = mpsc::channel(8);
        buffer_tx.send(Ok(0..=5)).await.unwrap();
        buffer_tx.send(Ok(1..=10)).await.unwrap();
        buffer_tx.send(Ok(20..=25)).await.unwrap();
        drop(buffer_tx);

        let (out_tx, mut out_rx) = mpsc::channel(8);
        Service::<Ethereum>::process_live_block_buffer(buffer_rx, out_tx, cutoff).await;

        let mut forwarded = Vec::new();
        while let Some(result) = out_rx.recv().await {
            forwarded.push(result.unwrap());
        }

        // All ranges should be forwarded since they're all >= 0
        assert_eq!(forwarded, vec![0..=5, 1..=10, 20..=25]);
        Ok(())
    }

    #[tokio::test]
    async fn forwards_errors_to_subscribers() -> anyhow::Result<()> {
        let asserter = Asserter::new();
        let provider = mocked_provider(asserter);
        let (mut service, _cmd) = Service::new(test_config(), provider);

        let (tx, mut rx) = mpsc::channel(1);
        service.subscriber = Some(tx);

        service.send_to_subscriber(Err(Error::WebSocketConnectionFailed(4))).await;

        match rx.recv().await.expect("subscriber should stay open") {
            Err(Error::WebSocketConnectionFailed(attempts)) => assert_eq!(attempts, 4),
            other => panic!("unexpected message: {other:?}"),
        }

        Ok(())
    }
}
