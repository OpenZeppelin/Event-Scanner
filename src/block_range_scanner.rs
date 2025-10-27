//! Example usage:
//!
//! ```rust,no_run
//! use alloy::{eips::BlockNumberOrTag, network::Ethereum, primitives::BlockNumber};
//! use std::ops::RangeInclusive;
//! use tokio_stream::{StreamExt, wrappers::ReceiverStream};
//!
//! use alloy::transports::http::reqwest::Url;
//! use event_scanner::{
//!     ScannerError,
//!     block_range_scanner::{
//!         BlockRangeScanner, BlockRangeScannerClient, DEFAULT_BLOCK_CONFIRMATIONS,
//!         DEFAULT_MAX_BLOCK_RANGE, Message,
//!     },
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
//!             Message::Data(range) => {
//!                 // process range
//!             }
//!             Message::Error(e) => {
//!                 error!("Received error from subscription: {e}");
//!                 match e {
//!                     ScannerError::ServiceShutdown => break,
//!                     ScannerError::WebSocketConnectionFailed(_) => {
//!                         error!(
//!                             "WebSocket connection failed, continuing to listen for reconnection"
//!                         );
//!                     }
//!                     _ => {
//!                         error!("Non-fatal error, continuing: {e}");
//!                     }
//!                 }
//!             }
//!             Message::Status(status) => {
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

use std::{cmp::Ordering, ops::RangeInclusive};
use tokio::{
    join,
    sync::{mpsc, oneshot},
};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};

use crate::{
    ScannerMessage,
    error::ScannerError,
    types::{ScannerStatus, TryStream},
};
use alloy::{
    consensus::BlockHeader,
    eips::BlockNumberOrTag,
    network::{BlockResponse, Network, primitives::HeaderResponse},
    primitives::{B256, BlockNumber},
    providers::{Provider, RootProvider},
    pubsub::Subscription,
    rpc::client::ClientBuilder,
    transports::{
        RpcError, TransportErrorKind, TransportResult, http::reqwest::Url, ws::WsConnect,
    },
};
use tracing::{debug, error, info, warn};

pub type Message = ScannerMessage<RangeInclusive<BlockNumber>, ScannerError>;

impl From<RangeInclusive<BlockNumber>> for Message {
    fn from(logs: RangeInclusive<BlockNumber>) -> Self {
        Message::Data(logs)
    }
}

impl PartialEq<RangeInclusive<BlockNumber>> for Message {
    fn eq(&self, other: &RangeInclusive<BlockNumber>) -> bool {
        if let Message::Data(range) = self { range.eq(other) } else { false }
    }
}

impl From<RpcError<TransportErrorKind>> for Message {
    fn from(error: RpcError<TransportErrorKind>) -> Self {
        Message::Error(error.into())
    }
}

impl From<ScannerError> for Message {
    fn from(error: ScannerError) -> Self {
        Message::Error(error)
    }
}

pub const DEFAULT_MAX_BLOCK_RANGE: u64 = 1000;
// copied form https://github.com/taikoxyz/taiko-mono/blob/f4b3a0e830e42e2fee54829326389709dd422098/packages/taiko-client/pkg/chain_iterator/block_batch_iterator.go#L19
pub const DEFAULT_BLOCK_CONFIRMATIONS: u64 = 0;

pub const MAX_BUFFERED_MESSAGES: usize = 50000;

// Maximum amount of reorged blocks on Ethereum (after this amount of block confirmations, a block
// is considered final)
pub const DEFAULT_REORG_REWIND_DEPTH: u64 = 64;

#[derive(Clone, Copy)]
pub struct BlockRangeScanner {
    pub max_block_range: u64,
}

impl Default for BlockRangeScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockRangeScanner {
    #[must_use]
    pub fn new() -> Self {
        Self { max_block_range: DEFAULT_MAX_BLOCK_RANGE }
    }

    #[must_use]
    pub fn max_block_range(mut self, max_block_range: u64) -> Self {
        self.max_block_range = max_block_range;
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
        Ok(self.connect(provider))
    }

    /// Connects to the provider via IPC
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> Result<ConnectedBlockRangeScanner<N>, RpcError<TransportErrorKind>> {
        let provider = RootProvider::<N>::new(ClientBuilder::default().ipc(ipc_path.into()).await?);
        Ok(self.connect(provider))
    }

    /// Connects to an existing provider
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    #[must_use]
    pub fn connect<N: Network>(self, provider: RootProvider<N>) -> ConnectedBlockRangeScanner<N> {
        ConnectedBlockRangeScanner { provider, max_block_range: self.max_block_range }
    }
}

pub struct ConnectedBlockRangeScanner<N: Network> {
    provider: RootProvider<N>,
    max_block_range: u64,
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
    pub fn run(&self) -> Result<BlockRangeScannerClient, ScannerError> {
        let (service, cmd_tx) = Service::new(self.provider.clone(), self.max_block_range);
        tokio::spawn(async move {
            service.run().await;
        });
        Ok(BlockRangeScannerClient::new(cmd_tx))
    }
}

#[derive(Debug)]
pub enum Command {
    StreamLive {
        subscriber: mpsc::Sender<Message>,
        block_confirmations: u64,
        response: oneshot::Sender<Result<(), ScannerError>>,
    },
    StreamHistorical {
        subscriber: mpsc::Sender<Message>,
        start_height: BlockNumberOrTag,
        end_height: BlockNumberOrTag,
        response: oneshot::Sender<Result<(), ScannerError>>,
    },
    StreamFrom {
        subscriber: mpsc::Sender<Message>,
        start_height: BlockNumberOrTag,
        block_confirmations: u64,
        response: oneshot::Sender<Result<(), ScannerError>>,
    },
    Rewind {
        subscriber: mpsc::Sender<Message>,
        start_height: BlockNumberOrTag,
        end_height: BlockNumberOrTag,
        response: oneshot::Sender<Result<(), ScannerError>>,
    },
}

struct Service<N: Network> {
    provider: RootProvider<N>,
    max_block_range: u64,
    error_count: u64,
    command_receiver: mpsc::Receiver<Command>,
    shutdown: bool,
}

impl<N: Network> Service<N> {
    pub fn new(provider: RootProvider<N>, max_block_range: u64) -> (Self, mpsc::Sender<Command>) {
        let (cmd_tx, cmd_rx) = mpsc::channel(100);

        let service = Self {
            provider,
            max_block_range,
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
                        warn!("Command channel closed, shutting down");
                        break;
                    }
                }
            }
        }

        info!("Subscription service stopped");
    }

    async fn handle_command(&mut self, command: Command) -> Result<(), ScannerError> {
        match command {
            Command::StreamLive { subscriber: sender, block_confirmations, response } => {
                info!("Starting live stream");
                let result = self.handle_live(block_confirmations, sender).await;
                let _ = response.send(result);
            }
            Command::StreamHistorical {
                subscriber: sender,
                start_height,
                end_height,
                response,
            } => {
                info!(start_height = ?start_height, end_height = ?end_height, "Starting historical stream");
                let result = self.handle_historical(start_height, end_height, sender).await;
                let _ = response.send(result);
            }
            Command::StreamFrom {
                subscriber: sender,
                start_height,
                block_confirmations,
                response,
            } => {
                info!(start_height = ?start_height, "Starting streaming from");
                let result = self.handle_sync(start_height, block_confirmations, sender).await;
                let _ = response.send(result);
            }
            Command::Rewind { subscriber: sender, start_height, end_height, response } => {
                info!(start_height = ?start_height, end_height = ?end_height, "Starting rewind");
                let result = self.handle_rewind(start_height, end_height, sender).await;
                let _ = response.send(result);
            }
        }
        Ok(())
    }

    async fn handle_live(
        &mut self,
        block_confirmations: u64,
        subscriber: mpsc::Sender<Message>,
    ) -> Result<(), ScannerError> {
        let max_block_range = self.max_block_range;
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
                subscriber,
                block_confirmations,
                max_block_range,
            )
            .await;
        });

        Ok(())
    }

    async fn handle_historical(
        &mut self,
        start_height: BlockNumberOrTag,
        end_height: BlockNumberOrTag,
        subscriber: mpsc::Sender<Message>,
    ) -> Result<(), ScannerError> {
        let max_block_range = self.max_block_range;

        let (start_block, end_block) = tokio::try_join!(
            self.provider.get_block_by_number(start_height),
            self.provider.get_block_by_number(end_height)
        )?;

        let start_block_num =
            start_block.ok_or_else(|| ScannerError::BlockNotFound(start_height))?.header().number();
        let end_block_num =
            end_block.ok_or_else(|| ScannerError::BlockNotFound(end_height))?.header().number();

        let (start_block_num, end_block_num) = match start_block_num.cmp(&end_block_num) {
            Ordering::Greater => (end_block_num, start_block_num),
            _ => (start_block_num, end_block_num),
        };

        info!(start_block = start_block_num, end_block = end_block_num, "Syncing historical data");

        tokio::spawn(async move {
            Self::stream_historical_blocks(
                start_block_num,
                end_block_num,
                max_block_range,
                &subscriber,
            )
            .await;
        });

        Ok(())
    }

    async fn handle_sync(
        &mut self,
        start_height: BlockNumberOrTag,
        block_confirmations: u64,
        subscriber: mpsc::Sender<Message>,
    ) -> Result<(), ScannerError> {
        let max_block_range = self.max_block_range;

        // Step 1:
        // Fetches the starting block and end block for historical sync in parallel
        let (start_block, latest_block) = tokio::try_join!(
            self.provider.get_block_by_number(start_height),
            self.provider.get_block_by_number(BlockNumberOrTag::Latest)
        )?;

        let start_block_num =
            start_block.ok_or_else(|| ScannerError::BlockNotFound(start_height))?.header().number();
        let latest_block = latest_block
            .ok_or_else(|| ScannerError::BlockNotFound(BlockNumberOrTag::Latest))?
            .header()
            .number();

        let confirmed_tip_num = latest_block.saturating_sub(block_confirmations);

        // If start is beyond confirmed tip, skip historical and go straight to live
        if start_block_num > confirmed_tip_num {
            info!(
                start_block = start_block_num,
                confirmed_tip = confirmed_tip_num,
                "Start block is beyond confirmed tip, starting live stream"
            );

            let provider = self.provider.clone();
            tokio::spawn(async move {
                Self::stream_live_blocks(
                    start_block_num,
                    provider,
                    subscriber,
                    block_confirmations,
                    max_block_range,
                )
                .await;
            });

            return Ok(());
        }

        info!(
            start_block = start_block_num,
            end_block = confirmed_tip_num,
            "Syncing historical data"
        );

        // Step 2: Setup the live streaming buffer
        // This channel will accumulate while historical sync is running
        let (live_block_buffer_sender, live_block_buffer_receiver) =
            mpsc::channel::<Message>(MAX_BUFFERED_MESSAGES);

        let provider = self.provider.clone();

        // The cutoff is the last block we have synced historically
        // Any block > cutoff will come from the live stream
        let cutoff = confirmed_tip_num;

        // This task runs independently, accumulating new blocks while wehistorical data is syncing
        tokio::spawn(async move {
            Self::stream_live_blocks(
                cutoff + 1,
                provider,
                live_block_buffer_sender,
                block_confirmations,
                max_block_range,
            )
            .await;
        });

        tokio::spawn(async move {
            // Step 4: Perform historical synchronization
            // This processes blocks from start_block to end_block (cutoff)
            // If this fails, we need to abort the live streaming task
            Self::stream_historical_blocks(
                start_block_num,
                confirmed_tip_num,
                max_block_range,
                &subscriber,
            )
            .await;

            info!("Chain tip reached, switching to live");
            if !subscriber.try_stream(ScannerStatus::ChainTipReached).await {
                return;
            }

            // Step 5:
            // Spawn the buffer processor task
            // This will:
            // 1. Process all buffered blocks, filtering out any ≤ cutoff
            // 2. Forward blocks > cutoff to the user
            // 3. Continue forwarding until the buffer if exhausted (waits for new blocks from live
            //    stream)
            Self::process_live_block_buffer(live_block_buffer_receiver, subscriber, cutoff).await;
        });

        info!("Successfully transitioned from historical to live data");

        Ok(())
    }

    async fn handle_rewind(
        &mut self,
        start_height: BlockNumberOrTag,
        end_height: BlockNumberOrTag,
        subscriber: mpsc::Sender<Message>,
    ) -> Result<(), ScannerError> {
        let max_block_range = self.max_block_range;
        let provider = self.provider.clone();

        let (start_block, end_block) = join!(
            self.provider.get_block_by_number(start_height),
            self.provider.get_block_by_number(end_height),
        );

        let start_block = start_block?.ok_or(ScannerError::BlockNotFound(start_height))?;
        let end_block = end_block?.ok_or(ScannerError::BlockNotFound(end_height))?;

        // normalize block range
        let (from, to) = match start_block.header().number().cmp(&end_block.header().number()) {
            Ordering::Greater => (start_block, end_block),
            _ => (end_block, start_block),
        };

        tokio::spawn(async move {
            Self::stream_rewind(from, to, max_block_range, &subscriber, &provider).await;
        });

        Ok(())
    }

    /// Streams blocks in reverse order from `from` to `to`.
    ///
    /// The `from` block is assumed to be greater than or equal to the `to` block.
    ///
    /// # Errors
    ///
    /// Returns an error if the stream fails
    async fn stream_rewind(
        from: N::BlockResponse,
        to: N::BlockResponse,
        max_block_range: u64,
        subscriber: &mpsc::Sender<Message>,
        provider: &RootProvider<N>,
    ) {
        let mut batch_count = 0;

        // for checking whether reorg occurred
        let mut tip_hash = from.header().hash();

        let from = from.header().number();
        let to = to.header().number();

        // we're iterating in reverse
        let mut batch_from = from;

        while batch_from >= to {
            let batch_to = batch_from.saturating_sub(max_block_range - 1).max(to);

            // stream the range regularly, i.e. from smaller block number to greater
            if !subscriber.try_stream(batch_to..=batch_from).await {
                break;
            }

            batch_count += 1;
            if batch_count % 10 == 0 {
                debug!(batch_count = batch_count, "Processed rewind batches");
            }

            // check early if end of stream achieved to avoid subtraction overflow when `to
            // == 0`
            if batch_to == to {
                break;
            }

            let reorged = match reorg_detected(provider, tip_hash).await {
                Ok(detected) => detected,
                Err(e) => {
                    error!(error = %e, "Terminal RPC call error, shutting down");
                    _ = subscriber.try_stream(e);
                    return;
                }
            };

            if reorged {
                info!(block_number = %from, hash = %tip_hash, "Reorg detected");

                if !subscriber.try_stream(ScannerStatus::ReorgDetected).await {
                    break;
                }

                // restart rewind
                batch_from = from;
                // store the updated end block hash
                tip_hash = match provider.get_block_by_number(from.into()).await {
                    Ok(block) => {
                        block.expect("Chain should have the same height post-reorg").header().hash()
                    }
                    Err(e) => {
                        error!(error = %e, "Terminal RPC call error, shutting down");
                        _ = subscriber.try_stream(e);
                        return;
                    }
                };
            } else {
                // `batch_to` is always greater than `to`, so `batch_to - 1` is always a valid
                // unsigned integer
                batch_from = batch_to - 1;
            }
        }

        info!(batch_count = batch_count, "Rewind completed");
    }

    async fn stream_historical_blocks(
        start: BlockNumber,
        end: BlockNumber,
        max_block_range: u64,
        subscriber: &mpsc::Sender<Message>,
    ) {
        let mut batch_count = 0;

        let mut next_start_block = start;

        // must be <= to include the edge case when start == end (i.e. return the single block
        // range)
        while next_start_block <= end {
            let batch_end_block_number =
                next_start_block.saturating_add(max_block_range - 1).min(end);

            if !subscriber.try_stream(next_start_block..=batch_end_block_number).await {
                break;
            }

            batch_count += 1;
            if batch_count % 10 == 0 {
                debug!(batch_count = batch_count, "Processed historical batches");
            }

            if batch_end_block_number == end {
                break;
            }

            // Next block number always exists as we checked end block previously
            let next_start_block_number = batch_end_block_number.saturating_add(1);

            next_start_block = next_start_block_number;
        }

        info!(batch_count = batch_count, "Historical sync completed");
    }

    async fn stream_live_blocks<P: Provider<N>>(
        mut range_start: BlockNumber,
        provider: P,
        subscriber: mpsc::Sender<Message>,
        block_confirmations: u64,
        max_block_range: u64,
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
                        if !subscriber.try_stream(ScannerStatus::ReorgDetected).await {
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
                        let range_end =
                            confirmed.min(range_start.saturating_add(max_block_range - 1));

                        if !subscriber.try_stream(range_start..=range_end).await {
                            return;
                        }

                        // Overflow can not realistically happen
                        range_start = range_end + 1;
                    }
                }
            }
            Err(e) => {
                _ = subscriber.try_stream(e).await;
            }
        }
    }

    async fn process_live_block_buffer(
        mut buffer_rx: mpsc::Receiver<Message>,
        subscriber: mpsc::Sender<Message>,
        cutoff: BlockNumber,
    ) {
        let mut processed = 0;
        let mut discarded = 0;

        // Process all buffered messages
        while let Some(data) = buffer_rx.recv().await {
            match data {
                Message::Data(range) => {
                    let (start, end) = (*range.start(), *range.end());
                    if start >= cutoff {
                        if !subscriber.try_stream(range).await {
                            break;
                        }
                        processed += end - start;
                    } else if end >= cutoff {
                        discarded += cutoff - start;

                        let start = cutoff;
                        if !subscriber.try_stream(start..=end).await {
                            break;
                        }
                        processed += end - start;
                    } else {
                        discarded += end - start;
                    }
                }
                other => {
                    // Could be error or status
                    if !subscriber.try_stream(other).await {
                        break;
                    }
                }
            }
        }

        info!(processed = processed, discarded = discarded, "Processed buffered messages");
    }

    async fn get_block_subscription(
        provider: &impl Provider<N>,
    ) -> Result<Subscription<N::HeaderResponse>, ScannerError> {
        let ws_stream = provider
            .subscribe_blocks()
            .await
            .map_err(|_| ScannerError::WebSocketConnectionFailed(1))?;

        Ok(ws_stream)
    }
}

async fn reorg_detected<N: Network>(
    provider: &RootProvider<N>,
    hash_to_check: B256,
) -> Result<bool, RpcError<TransportErrorKind>> {
    Ok(provider.get_block_by_hash(hash_to_check).await?.is_none())
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
    /// * `ScannerError::ServiceShutdown` - if the service is already shutting down.
    pub async fn stream_live(
        &self,
        block_confirmations: u64,
    ) -> Result<ReceiverStream<Message>, ScannerError> {
        let (blocks_sender, blocks_receiver) = mpsc::channel(MAX_BUFFERED_MESSAGES);
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::StreamLive {
            subscriber: blocks_sender,
            block_confirmations,
            response: response_tx,
        };

        self.command_sender.send(command).await.map_err(|_| ScannerError::ServiceShutdown)?;

        response_rx.await.map_err(|_| ScannerError::ServiceShutdown)??;

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
    /// * `ScannerError::ServiceShutdown` - if the service is already shutting down.
    pub async fn stream_historical<N: Into<BlockNumberOrTag>>(
        &self,
        start_height: N,
        end_height: N,
    ) -> Result<ReceiverStream<Message>, ScannerError> {
        let (blocks_sender, blocks_receiver) = mpsc::channel(MAX_BUFFERED_MESSAGES);
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::StreamHistorical {
            subscriber: blocks_sender,
            start_height: start_height.into(),
            end_height: end_height.into(),
            response: response_tx,
        };

        self.command_sender.send(command).await.map_err(|_| ScannerError::ServiceShutdown)?;

        response_rx.await.map_err(|_| ScannerError::ServiceShutdown)??;

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
    /// * `ScannerError::ServiceShutdown` - if the service is already shutting down.
    pub async fn stream_from(
        &self,
        start_height: BlockNumberOrTag,
        block_confirmations: u64,
    ) -> Result<ReceiverStream<Message>, ScannerError> {
        let (blocks_sender, blocks_receiver) = mpsc::channel(MAX_BUFFERED_MESSAGES);
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::StreamFrom {
            subscriber: blocks_sender,
            start_height,
            block_confirmations,
            response: response_tx,
        };

        self.command_sender.send(command).await.map_err(|_| ScannerError::ServiceShutdown)?;

        response_rx.await.map_err(|_| ScannerError::ServiceShutdown)??;

        Ok(ReceiverStream::new(blocks_receiver))
    }

    /// Streams blocks in reverse order from `start_height` to `end_height`.
    ///
    /// # Arguments
    ///
    /// * `start_height` - The starting block number or tag (defaults to Latest if None).
    /// * `end_height` - The ending block number or tag (defaults to Earliest if None).
    ///
    /// # Errors
    ///
    /// * `ScannerError::ServiceShutdown` - if the service is already shutting down.
    pub async fn rewind<BN: Into<BlockNumberOrTag>>(
        &self,
        start_height: BN,
        end_height: BN,
    ) -> Result<ReceiverStream<Message>, ScannerError> {
        let (blocks_sender, blocks_receiver) = mpsc::channel(MAX_BUFFERED_MESSAGES);
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::Rewind {
            subscriber: blocks_sender,
            start_height: start_height.into(),
            end_height: end_height.into(),
            response: response_tx,
        };

        self.command_sender.send(command).await.map_err(|_| ScannerError::ServiceShutdown)?;

        response_rx.await.map_err(|_| ScannerError::ServiceShutdown)??;

        Ok(ReceiverStream::new(blocks_receiver))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{assert_closed, assert_empty, assert_next};
    use alloy::{
        network::Ethereum,
        providers::{ProviderBuilder, ext::AnvilApi},
        rpc::types::anvil::ReorgOptions,
    };
    use alloy_node_bindings::Anvil;
    use tokio::sync::mpsc;
    use tokio_stream::StreamExt;

    #[test]
    fn block_range_scanner_defaults_match_constants() {
        let scanner = BlockRangeScanner::new();

        assert_eq!(scanner.max_block_range, DEFAULT_MAX_BLOCK_RANGE);
    }

    #[test]
    fn builder_methods_update_configuration() {
        let max_block_range = 42;

        let scanner = BlockRangeScanner::new().max_block_range(max_block_range);

        assert_eq!(scanner.max_block_range, max_block_range);
    }

    #[tokio::test]
    async fn live_mode_processes_all_blocks_respecting_block_confirmations() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn()?;
        let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

        // --- Zero block confirmations -> stream immediately ---

        let client = BlockRangeScanner::new()
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        let mut stream = client.stream_live(0).await?;

        provider.anvil_mine(Some(5), None).await?;

        assert_next!(stream, 1..=1);
        assert_next!(stream, 2..=2);
        assert_next!(stream, 3..=3);
        assert_next!(stream, 4..=4);
        assert_next!(stream, 5..=5);
        let mut stream = assert_empty!(stream);

        provider.anvil_mine(Some(1), None).await?;

        assert_next!(stream, 6..=6);
        assert_empty!(stream);

        // --- 1 block confirmation  ---

        let mut stream = client.stream_live(1).await?;

        provider.anvil_mine(Some(5), None).await?;

        assert_next!(stream, 6..=6);
        assert_next!(stream, 7..=7);
        assert_next!(stream, 8..=8);
        assert_next!(stream, 9..=9);
        assert_next!(stream, 10..=10);
        let mut stream = assert_empty!(stream);

        provider.anvil_mine(Some(1), None).await?;

        assert_next!(stream, 11..=11);
        assert_empty!(stream);

        Ok(())
    }

    #[tokio::test]
    async fn stream_from_latest_starts_at_tip_not_confirmed() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn()?;

        let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;
        provider.anvil_mine(Some(20), None).await?;

        let block_confirmations = 5;

        let client = BlockRangeScanner::new()
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        let stream = client.stream_from(BlockNumberOrTag::Latest, block_confirmations).await?;

        let stream = assert_empty!(stream);

        provider.anvil_mine(Some(4), None).await?;

        let mut stream = assert_empty!(stream);

        provider.anvil_mine(Some(1), None).await?;

        assert_next!(stream, 20..=20);
        let mut stream = assert_empty!(stream);

        provider.anvil_mine(Some(1), None).await?;

        assert_next!(stream, 21..=21);
        assert_empty!(stream);

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

        provider.anvil_mine(Some(10), None).await?;

        provider
            .anvil_reorg(ReorgOptions { depth: block_confirmations - 1, tx_block_pairs: vec![] })
            .await?;

        provider.anvil_mine(Some(20), None).await?;

        let mut block_range_start = 0;

        let end_loop = 20;
        let mut i = 0;
        while let Some(Message::Data(range)) = receiver.next().await {
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

        provider.anvil_mine(Some(10), None).await?;

        provider
            .anvil_reorg(ReorgOptions { depth: block_confirmations + 5, tx_block_pairs: vec![] })
            .await?;

        provider.anvil_mine(Some(30), None).await?;
        receiver.close();

        let mut block_range_start = 0;

        let mut block_num = vec![];
        let mut reorg_detected = false;
        while let Some(msg) = receiver.next().await {
            match msg {
                Message::Data(range) => {
                    if block_range_start == 0 {
                        block_range_start = *range.start();
                    }
                    block_num.push(range);
                    if block_num.len() == 15 {
                        break;
                    }
                }
                Message::Status(ScannerStatus::ReorgDetected) => {
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
    #[ignore = "too flaky, un-ignore once a full local node is used: https://github.com/OpenZeppelin/Event-Scanner/issues/109"]
    async fn historical_emits_correction_range_when_reorg_below_end() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn()?;
        let provider = ProviderBuilder::new().connect(anvil.ws_endpoint_url().as_str()).await?;

        provider.anvil_mine(Some(120), None).await?;

        let end_num = 110;

        let client = BlockRangeScanner::new()
            .max_block_range(30)
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        let mut stream = client
            .stream_historical(BlockNumberOrTag::Number(0), BlockNumberOrTag::Number(end_num))
            .await?;

        let depth = 15;
        _ = provider.anvil_reorg(ReorgOptions { depth, tx_block_pairs: vec![] }).await;
        _ = provider.anvil_mine(Some(20), None).await;

        assert_next!(stream, 0..=29);
        assert_next!(stream, 30..=59);
        assert_next!(stream, 60..=89);
        assert_next!(stream, 90..=110);
        assert_next!(stream, ScannerStatus::ReorgDetected);
        assert_next!(stream, 105..=110);
        assert_closed!(stream);

        Ok(())
    }

    #[tokio::test]
    #[ignore = "too flaky, un-ignore once a full local node is used: https://github.com/OpenZeppelin/Event-Scanner/issues/109"]
    async fn historical_emits_correction_range_when_end_num_reorgs() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn()?;
        let provider = ProviderBuilder::new().connect(anvil.ws_endpoint_url().as_str()).await?;

        provider.anvil_mine(Some(120), None).await?;

        let end_num = 120;

        let client = BlockRangeScanner::new()
            .max_block_range(30)
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        let mut stream = client
            .stream_historical(BlockNumberOrTag::Number(0), BlockNumberOrTag::Number(end_num))
            .await?;

        let pre_reorg_mine = 20;
        _ = provider.anvil_mine(Some(pre_reorg_mine), None).await;
        let depth = pre_reorg_mine + 1;
        _ = provider.anvil_reorg(ReorgOptions { depth, tx_block_pairs: vec![] }).await;
        _ = provider.anvil_mine(Some(20), None).await;

        assert_next!(stream, 0..=29);
        assert_next!(stream, 30..=59);
        assert_next!(stream, 60..=89);
        assert_next!(stream, 90..=120);
        assert_next!(stream, ScannerStatus::ReorgDetected);
        assert_next!(stream, 120..=120);
        assert_closed!(stream);

        Ok(())
    }

    #[tokio::test]
    async fn historic_mode_respects_blocks_read_per_epoch() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn()?;

        let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

        provider.anvil_mine(Some(100), None).await?;

        let client = BlockRangeScanner::new()
            .max_block_range(5)
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        // ranges where each batch is of max blocks per epoch size
        let mut stream = client.stream_historical(0, 19).await?;
        assert_next!(stream, 0..=4);
        assert_next!(stream, 5..=9);
        assert_next!(stream, 10..=14);
        assert_next!(stream, 15..=19);
        assert_closed!(stream);

        // ranges where last batch is smaller than blocks per epoch
        let mut stream = client.stream_historical(93, 99).await?;
        assert_next!(stream, 93..=97);
        assert_next!(stream, 98..=99);
        assert_closed!(stream);

        // range where blocks per epoch is larger than the number of blocks in the range
        let mut stream = client.stream_historical(3, 5).await?;
        assert_next!(stream, 3..=5);
        assert_closed!(stream);

        // single item range
        let mut stream = client.stream_historical(3, 3).await?;
        assert_next!(stream, 3..=3);
        assert_closed!(stream);

        // range where blocks per epoch is larger than the number of blocks on chain
        let client = BlockRangeScanner::new()
            .max_block_range(200)
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        let mut stream = client.stream_historical(0, 20).await?;
        assert_next!(stream, 0..=20);
        assert_closed!(stream);

        let mut stream = client.stream_historical(0, 99).await?;
        assert_next!(stream, 0..=99);
        assert_closed!(stream);

        Ok(())
    }

    #[tokio::test]
    async fn historic_mode_normalises_start_and_end_block() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn()?;

        let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;
        provider.anvil_mine(Some(11), None).await?;

        let client = BlockRangeScanner::new()
            .max_block_range(5)
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        let mut stream = client.stream_historical(10, 0).await?;
        assert_next!(stream, 0..=4);
        assert_next!(stream, 5..=9);
        assert_next!(stream, 10..=10);
        assert_closed!(stream);

        Ok(())
    }

    #[tokio::test]
    async fn buffered_messages_after_cutoff_are_all_passed() {
        let cutoff = 50;
        let (buffer_tx, buffer_rx) = mpsc::channel(8);
        buffer_tx.send(Message::Data(51..=55)).await.unwrap();
        buffer_tx.send(Message::Data(56..=60)).await.unwrap();
        buffer_tx.send(Message::Data(61..=70)).await.unwrap();
        drop(buffer_tx);

        let (out_tx, out_rx) = mpsc::channel(8);
        Service::<Ethereum>::process_live_block_buffer(buffer_rx, out_tx, cutoff).await;

        let mut stream = ReceiverStream::new(out_rx);

        assert_next!(stream, 51..=55);
        assert_next!(stream, 56..=60);
        assert_next!(stream, 61..=70);
        assert_closed!(stream);
    }

    #[tokio::test]
    async fn ranges_entirely_before_cutoff_are_discarded() {
        let cutoff = 100;

        let (buffer_tx, buffer_rx) = mpsc::channel(8);
        buffer_tx.send(Message::Data(40..=50)).await.unwrap();
        buffer_tx.send(Message::Data(51..=60)).await.unwrap();
        buffer_tx.send(Message::Data(61..=70)).await.unwrap();
        drop(buffer_tx);

        let (out_tx, out_rx) = mpsc::channel(8);
        Service::<Ethereum>::process_live_block_buffer(buffer_rx, out_tx, cutoff).await;

        let mut stream = ReceiverStream::new(out_rx);

        assert_closed!(stream);
    }

    #[tokio::test]
    async fn ranges_overlapping_cutoff_are_trimmed() {
        let cutoff = 75;

        let (buffer_tx, buffer_rx) = mpsc::channel(8);
        buffer_tx.send(Message::Data(60..=70)).await.unwrap();
        buffer_tx.send(Message::Data(71..=80)).await.unwrap();
        buffer_tx.send(Message::Data(81..=86)).await.unwrap();
        drop(buffer_tx);

        let (out_tx, out_rx) = mpsc::channel(8);
        Service::<Ethereum>::process_live_block_buffer(buffer_rx, out_tx, cutoff).await;

        let mut stream = ReceiverStream::new(out_rx);

        assert_next!(stream, 75..=80);
        assert_next!(stream, 81..=86);
        assert_closed!(stream);
    }

    #[tokio::test]
    async fn edge_case_range_exactly_at_cutoff() {
        let cutoff = 100;

        let (buffer_tx, buffer_rx) = mpsc::channel(8);
        buffer_tx.send(Message::Data(98..=98)).await.unwrap(); // Just before: discard
        buffer_tx.send(Message::Data(99..=100)).await.unwrap(); // Includes cutoff: trim to 100..=100
        buffer_tx.send(Message::Data(100..=100)).await.unwrap(); // Exactly at: forward
        buffer_tx.send(Message::Data(100..=101)).await.unwrap(); // Starts at cutoff: forward
        buffer_tx.send(Message::Data(102..=102)).await.unwrap(); // After cutoff: forward
        drop(buffer_tx);

        let (out_tx, out_rx) = mpsc::channel(8);
        Service::<Ethereum>::process_live_block_buffer(buffer_rx, out_tx, cutoff).await;

        let mut stream = ReceiverStream::new(out_rx);

        assert_next!(stream, 100..=100);
        assert_next!(stream, 100..=100);
        assert_next!(stream, 100..=101);
        assert_next!(stream, 102..=102);
        assert_closed!(stream);
    }

    #[tokio::test]
    async fn try_send_forwards_errors_to_subscribers() {
        let (tx, mut rx) = mpsc::channel(1);

        _ = tx.try_stream(ScannerError::WebSocketConnectionFailed(4)).await;

        assert!(matches!(
            rx.recv().await,
            Some(Message::Error(ScannerError::WebSocketConnectionFailed(4)))
        ));
    }

    #[tokio::test]
    async fn rewind_single_batch_when_epoch_larger_than_range() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn()?;

        let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

        provider.anvil_mine(Some(150), None).await?;

        let client = BlockRangeScanner::new()
            .max_block_range(100)
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        let mut stream = client.rewind(100, 150).await?;

        // Range length is 51, epoch is 100 -> single batch [100..=150]
        assert_next!(stream, 100..=150);
        assert_closed!(stream);

        Ok(())
    }

    #[tokio::test]
    async fn rewind_exact_multiple_of_epoch_creates_full_batches_in_reverse() -> anyhow::Result<()>
    {
        let anvil = Anvil::new().try_spawn()?;

        let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

        provider.anvil_mine(Some(15), None).await?;

        let client = BlockRangeScanner::new()
            .max_block_range(5)
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        let mut stream = client.rewind(0, 14).await?;

        // 0..=14 with epoch 5 -> [10..=14, 5..=9, 0..=4]
        assert_next!(stream, 10..=14);
        assert_next!(stream, 5..=9);
        assert_next!(stream, 0..=4);
        assert_closed!(stream);

        Ok(())
    }

    #[tokio::test]
    async fn rewind_with_remainder_trims_first_batch_to_stream_start() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn()?;

        let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

        provider.anvil_mine(Some(15), None).await?;

        let client = BlockRangeScanner::new()
            .max_block_range(4)
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        let mut stream = client.rewind(3, 12).await?;

        // 3..=12 with epoch 4 -> ends: 12,8,4 -> batches: [9..=12, 5..=8, 3..=4]
        assert_next!(stream, 9..=12);
        assert_next!(stream, 5..=8);
        assert_next!(stream, 3..=4);
        assert_closed!(stream);

        Ok(())
    }

    #[tokio::test]
    async fn rewind_single_block_range() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn()?;

        let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

        provider.anvil_mine(Some(15), None).await?;

        let client = BlockRangeScanner::new()
            .max_block_range(5)
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        let mut stream = client.rewind(7, 7).await?;

        assert_next!(stream, 7..=7);
        assert_closed!(stream);

        Ok(())
    }

    #[tokio::test]
    async fn rewind_epoch_of_one_sends_each_block_in_reverse_order() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn()?;

        let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;

        provider.anvil_mine(Some(15), None).await?;

        let client = BlockRangeScanner::new()
            .max_block_range(1)
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        let mut stream = client.rewind(5, 8).await?;

        // 5..=8 with epoch 1 -> [8..=8, 7..=7, 6..=6, 5..=5]
        assert_next!(stream, 8..=8);
        assert_next!(stream, 7..=7);
        assert_next!(stream, 6..=6);
        assert_next!(stream, 5..=5);
        assert_closed!(stream);

        Ok(())
    }

    #[tokio::test]
    async fn command_rewind_defaults_latest_to_earliest_batches_correctly() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn()?;

        let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;
        // Mine 20 blocks, so the total number of blocks is 21 (including 0th block)
        provider.anvil_mine(Some(20), None).await?;

        let client = BlockRangeScanner::new()
            .max_block_range(7)
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        let mut stream = client
            .rewind::<BlockNumberOrTag>(BlockNumberOrTag::Earliest, BlockNumberOrTag::Latest)
            .await?;

        assert_next!(stream, 14..=20);
        assert_next!(stream, 7..=13);
        assert_next!(stream, 0..=6);
        assert_closed!(stream);

        Ok(())
    }

    #[tokio::test]
    async fn command_rewind_handles_start_and_end_in_any_order() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn()?;

        let provider = ProviderBuilder::new().connect(anvil.endpoint().as_str()).await?;
        // Ensure blocks at 3 and 15 exist
        provider.anvil_mine(Some(16), None).await?;

        let client = BlockRangeScanner::new()
            .max_block_range(5)
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        let mut stream = client.rewind(15, 3).await?;

        assert_next!(stream, 11..=15);
        assert_next!(stream, 6..=10);
        assert_next!(stream, 3..=5);
        assert_closed!(stream);

        let mut stream = client.rewind(3, 15).await?;

        assert_next!(stream, 11..=15);
        assert_next!(stream, 6..=10);
        assert_next!(stream, 3..=5);
        assert_closed!(stream);

        Ok(())
    }

    #[tokio::test]
    async fn command_rewind_propagates_block_not_found_error() -> anyhow::Result<()> {
        let anvil = Anvil::new().try_spawn()?;

        // Do not mine up to 999 so start won't exist
        let client = BlockRangeScanner::new()
            .max_block_range(5)
            .connect_ws::<Ethereum>(anvil.ws_endpoint_url())
            .await?
            .run()?;

        let stream = client.rewind(0, 999).await;

        assert!(matches!(stream, Err(ScannerError::BlockNotFound(BlockNumberOrTag::Number(999)))));

        Ok(())
    }
}
