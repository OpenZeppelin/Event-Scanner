//! Example usage:
//!
//! ```rust,no_run
//! use alloy::{eips::BlockNumberOrTag, network::Ethereum, primitives::BlockNumber};
//! use std::ops::RangeInclusive;
//! use tokio_stream::{StreamExt, wrappers::ReceiverStream};
//!
//! use alloy::providers::{Provider, ProviderBuilder};
//! use event_scanner::{
//!     ScannerError, ScannerMessage,
//!     block_range_scanner::{
//!         BlockRangeScanner, BlockRangeScannerClient, DEFAULT_BLOCK_CONFIRMATIONS,
//!         DEFAULT_MAX_BLOCK_RANGE,
//!     },
//!     robust_provider::RobustProviderBuilder,
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
//!     let provider = ProviderBuilder::new().connect("ws://localhost:8546").await?;
//!     let robust_provider = RobustProviderBuilder::new(provider).build().await?;
//!     let block_range_scanner = BlockRangeScanner::new().connect(robust_provider).await?;
//!
//!     // Create client to send subscribe command to block scanner
//!     let client: BlockRangeScannerClient = block_range_scanner.run()?;
//!
//!     let mut stream =
//!         client.stream_from(BlockNumberOrTag::Number(5), DEFAULT_BLOCK_CONFIRMATIONS).await?;
//!
//!     while let Some(message) = stream.next().await {
//!         match message {
//!             Ok(ScannerMessage::Data(range)) => {
//!                 // process range
//!             }
//!             Ok(ScannerMessage::Notification(notification)) => {
//!                 info!("Received notification: {:?}", notification);
//!             }
//!             Err(e) => {
//!                 error!("Received error from subscription: {e}");
//!                 match e {
//!                     ScannerError::ServiceShutdown => break,
//!                     _ => {
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

use std::{cmp::Ordering, ops::RangeInclusive};
use tokio::{
    sync::{mpsc, oneshot},
    try_join,
};
use tokio_stream::wrappers::ReceiverStream;

use crate::{
    ScannerError, ScannerMessage,
    block_range_scanner::sync_handler::SyncHandler,
    robust_provider::{IntoRobustProvider, RobustProvider, provider::Error as RobustProviderError},
    types::{IntoScannerResult, Notification, ScannerResult, TryStream},
};

use alloy::{
    consensus::BlockHeader,
    eips::BlockId,
    network::{BlockResponse, Network, primitives::HeaderResponse},
    primitives::BlockNumber,
};
use tracing::{debug, error, info, warn};

mod common;
mod reorg_handler;
mod ring_buffer;
mod sync_handler;

use reorg_handler::ReorgHandler;
pub use ring_buffer::RingBufferCapacity;

pub const DEFAULT_MAX_BLOCK_RANGE: u64 = 1000;
// copied form https://github.com/taikoxyz/taiko-mono/blob/f4b3a0e830e42e2fee54829326389709dd422098/packages/taiko-client/pkg/chain_iterator/block_batch_iterator.go#L19
pub const DEFAULT_BLOCK_CONFIRMATIONS: u64 = 0;

pub const MAX_BUFFERED_MESSAGES: usize = 50000;

// Maximum amount of reorged blocks on Ethereum (after this amount of block confirmations, a block
// is considered final)
pub const DEFAULT_REORG_REWIND_DEPTH: u64 = 64;

pub type BlockScannerResult = ScannerResult<RangeInclusive<BlockNumber>>;

pub type Message = ScannerMessage<RangeInclusive<BlockNumber>>;

impl From<RangeInclusive<BlockNumber>> for Message {
    fn from(range: RangeInclusive<BlockNumber>) -> Self {
        Message::Data(range)
    }
}

impl PartialEq<RangeInclusive<BlockNumber>> for Message {
    fn eq(&self, other: &RangeInclusive<BlockNumber>) -> bool {
        if let Message::Data(range) = self { range.eq(other) } else { false }
    }
}

impl IntoScannerResult<RangeInclusive<BlockNumber>> for RangeInclusive<BlockNumber> {
    fn into_scanner_message_result(self) -> BlockScannerResult {
        Ok(Message::Data(self))
    }
}

#[derive(Clone)]
pub struct BlockRangeScanner {
    pub max_block_range: u64,
    pub past_blocks_storage_capacity: RingBufferCapacity,
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
            max_block_range: DEFAULT_MAX_BLOCK_RANGE,
            past_blocks_storage_capacity: RingBufferCapacity::Limited(10),
        }
    }

    #[must_use]
    pub fn max_block_range(mut self, max_block_range: u64) -> Self {
        self.max_block_range = max_block_range;
        self
    }

    #[must_use]
    pub fn past_blocks_storage_capacity(
        mut self,
        past_blocks_storage_capacity: RingBufferCapacity,
    ) -> Self {
        self.past_blocks_storage_capacity = past_blocks_storage_capacity;
        self
    }

    /// Connects to an existing provider
    ///
    /// # Errors
    ///
    /// Returns an error if the provider connection fails.
    pub async fn connect<N: Network>(
        self,
        provider: impl IntoRobustProvider<N>,
    ) -> Result<ConnectedBlockRangeScanner<N>, ScannerError> {
        let provider = provider.into_robust_provider().await?;
        Ok(ConnectedBlockRangeScanner {
            provider,
            max_block_range: self.max_block_range,
            past_blocks_storage_capacity: self.past_blocks_storage_capacity,
        })
    }
}

pub struct ConnectedBlockRangeScanner<N: Network> {
    provider: RobustProvider<N>,
    max_block_range: u64,
    past_blocks_storage_capacity: RingBufferCapacity,
}

impl<N: Network> ConnectedBlockRangeScanner<N> {
    /// Returns the `RobustProvider`
    #[must_use]
    pub fn provider(&self) -> &RobustProvider<N> {
        &self.provider
    }

    /// Starts the subscription service and returns a client for sending commands.
    ///
    /// # Errors
    ///
    /// Returns an error if the subscription service fails to start.
    pub fn run(&self) -> Result<BlockRangeScannerClient, ScannerError> {
        let (service, cmd_tx) = Service::new(
            self.provider.clone(),
            self.max_block_range,
            self.past_blocks_storage_capacity,
        );
        tokio::spawn(async move {
            service.run().await;
        });
        Ok(BlockRangeScannerClient::new(cmd_tx))
    }
}

#[derive(Debug)]
pub enum Command {
    StreamLive {
        sender: mpsc::Sender<BlockScannerResult>,
        block_confirmations: u64,
        response: oneshot::Sender<Result<(), ScannerError>>,
    },
    StreamHistorical {
        sender: mpsc::Sender<BlockScannerResult>,
        start_id: BlockId,
        end_id: BlockId,
        response: oneshot::Sender<Result<(), ScannerError>>,
    },
    StreamFrom {
        sender: mpsc::Sender<BlockScannerResult>,
        start_id: BlockId,
        block_confirmations: u64,
        response: oneshot::Sender<Result<(), ScannerError>>,
    },
    Rewind {
        sender: mpsc::Sender<BlockScannerResult>,
        start_id: BlockId,
        end_id: BlockId,
        response: oneshot::Sender<Result<(), ScannerError>>,
    },
}

struct Service<N: Network> {
    provider: RobustProvider<N>,
    max_block_range: u64,
    past_blocks_storage_capacity: RingBufferCapacity,
    error_count: u64,
    command_receiver: mpsc::Receiver<Command>,
    shutdown: bool,
}

impl<N: Network> Service<N> {
    pub fn new(
        provider: RobustProvider<N>,
        max_block_range: u64,
        past_blocks_storage_capacity: RingBufferCapacity,
    ) -> (Self, mpsc::Sender<Command>) {
        let (cmd_tx, cmd_rx) = mpsc::channel(100);

        let service = Self {
            provider,
            max_block_range,
            past_blocks_storage_capacity,
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
            Command::StreamLive { sender, block_confirmations, response } => {
                info!("Starting live stream");
                let result = self.handle_live(block_confirmations, sender).await;
                let _ = response.send(result);
            }
            Command::StreamHistorical { sender, start_id, end_id, response } => {
                info!(start_id = ?start_id, end_id = ?end_id, "Starting historical stream");
                let result = self.handle_historical(start_id, end_id, sender).await;
                let _ = response.send(result);
            }
            Command::StreamFrom { sender, start_id, block_confirmations, response } => {
                info!(start_id = ?start_id, "Starting streaming from");
                let result = self.handle_sync(start_id, block_confirmations, sender).await;
                let _ = response.send(result);
            }
            Command::Rewind { sender, start_id, end_id, response } => {
                info!(start_id = ?start_id, end_id = ?end_id, "Starting rewind");
                let result = self.handle_rewind(start_id, end_id, sender).await;
                let _ = response.send(result);
            }
        }
        Ok(())
    }

    async fn handle_live(
        &mut self,
        block_confirmations: u64,
        sender: mpsc::Sender<BlockScannerResult>,
    ) -> Result<(), ScannerError> {
        let max_block_range = self.max_block_range;
        let past_blocks_storage_capacity = self.past_blocks_storage_capacity;
        let latest = self.provider.get_block_number().await?;
        let provider = self.provider.clone();

        // the next block returned by the underlying subscription will always be `latest + 1`,
        // because `latest` was already mined and subscription by definition only streams after new
        // blocks have been mined
        let range_start = (latest + 1).saturating_sub(block_confirmations);

        let subscription = self.provider.subscribe_blocks().await?;

        info!("WebSocket connected for live blocks");

        tokio::spawn(async move {
            let mut reorg_handler =
                ReorgHandler::new(provider.clone(), past_blocks_storage_capacity);

            common::stream_live_blocks(
                range_start,
                subscription,
                &sender,
                &provider,
                block_confirmations,
                max_block_range,
                &mut reorg_handler,
                false, // (notification unnecessary)
            )
            .await;
        });

        Ok(())
    }

    async fn handle_historical(
        &mut self,
        start_id: BlockId,
        end_id: BlockId,
        sender: mpsc::Sender<BlockScannerResult>,
    ) -> Result<(), ScannerError> {
        let max_block_range = self.max_block_range;
        let past_blocks_storage_capacity = self.past_blocks_storage_capacity;
        let provider = self.provider.clone();

        let (start_block, end_block) =
            tokio::try_join!(self.provider.get_block(start_id), self.provider.get_block(end_id))?;

        let start_block_num = start_block.header().number();
        let end_block_num = end_block.header().number();

        let (start_block_num, end_block_num) = match start_block_num.cmp(&end_block_num) {
            Ordering::Greater => (end_block_num, start_block_num),
            _ => (start_block_num, end_block_num),
        };

        info!(start_block = start_block_num, end_block = end_block_num, "Syncing historical data");

        tokio::spawn(async move {
            let mut reorg_handler =
                ReorgHandler::new(provider.clone(), past_blocks_storage_capacity);

            common::stream_block_range(
                start_block_num,
                start_block_num,
                end_block_num,
                max_block_range,
                &sender,
                &provider,
                &mut reorg_handler,
            )
            .await;
        });

        Ok(())
    }

    async fn handle_sync(
        &self,
        start_id: BlockId,
        block_confirmations: u64,
        sender: mpsc::Sender<BlockScannerResult>,
    ) -> Result<(), ScannerError> {
        let sync_handler = SyncHandler::new(
            self.provider.clone(),
            self.max_block_range,
            start_id,
            block_confirmations,
            self.past_blocks_storage_capacity,
            sender,
        );
        sync_handler.run().await
    }

    async fn handle_rewind(
        &mut self,
        start_id: BlockId,
        end_id: BlockId,
        sender: mpsc::Sender<BlockScannerResult>,
    ) -> Result<(), ScannerError> {
        let max_block_range = self.max_block_range;
        let past_blocks_storage_capacity = self.past_blocks_storage_capacity;
        let provider = self.provider.clone();

        let (start_block, end_block) =
            try_join!(self.provider.get_block(start_id), self.provider.get_block(end_id))?;

        // normalize block range
        let (from, to) = match start_block.header().number().cmp(&end_block.header().number()) {
            Ordering::Greater => (start_block, end_block),
            _ => (end_block, start_block),
        };

        tokio::spawn(async move {
            let mut reorg_handler =
                ReorgHandler::new(provider.clone(), past_blocks_storage_capacity);

            Self::stream_rewind(from, to, max_block_range, &sender, &provider, &mut reorg_handler)
                .await;
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
        sender: &mpsc::Sender<BlockScannerResult>,
        provider: &RobustProvider<N>,
        reorg_handler: &mut ReorgHandler<N>,
    ) {
        let mut batch_count = 0;

        // for checking whether reorg occurred
        let mut tip = from;

        let from = tip.header().number();
        let to = to.header().number();

        // we're iterating in reverse
        let mut batch_from = from;

        while batch_from >= to {
            let batch_to = batch_from.saturating_sub(max_block_range - 1).max(to);

            // stream the range regularly, i.e. from smaller block number to greater
            if !sender.try_stream(batch_to..=batch_from).await {
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

            let reorged_opt = match reorg_handler.check(&tip).await {
                Ok(opt) => {
                    info!(block_number = %from, hash = %tip.header().hash(), "Reorg detected");
                    opt
                }
                Err(e) => {
                    error!(error = %e, "Terminal RPC call error, shutting down");
                    _ = sender.try_stream(e).await;
                    return;
                }
            };

            // For now we only care if a reorg occurred, not which block it was.
            // Once we optimize 'latest' mode to update only the reorged logs, we will need the
            // exact common ancestor.
            if reorged_opt.is_some() {
                info!(block_number = %from, hash = %tip.header().hash(), "Reorg detected");

                if !sender.try_stream(Notification::ReorgDetected).await {
                    break;
                }

                // restart rewind
                batch_from = from;
                // store the updated end block hash
                tip = match provider.get_block_by_number(from.into()).await {
                    Ok(block) => block,
                    Err(RobustProviderError::BlockNotFound(_)) => {
                        panic!("Block with number '{from}' should exist post-reorg");
                    }
                    Err(e) => {
                        error!(error = %e, "Terminal RPC call error, shutting down");
                        _ = sender.try_stream(e).await;
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
    ) -> Result<ReceiverStream<BlockScannerResult>, ScannerError> {
        let (blocks_sender, blocks_receiver) = mpsc::channel(MAX_BUFFERED_MESSAGES);
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::StreamLive {
            sender: blocks_sender,
            block_confirmations,
            response: response_tx,
        };

        self.command_sender.send(command).await.map_err(|_| ScannerError::ServiceShutdown)?;

        response_rx.await.map_err(|_| ScannerError::ServiceShutdown)??;

        Ok(ReceiverStream::new(blocks_receiver))
    }

    /// Streams a batch of historical blocks from `start_id` to `end_id`.
    ///
    /// # Arguments
    ///
    /// * `start_id` - The starting block id
    /// * `end_id` - The ending block id
    ///
    /// # Errors
    ///
    /// * `ScannerError::ServiceShutdown` - if the service is already shutting down.
    pub async fn stream_historical(
        &self,
        start_id: impl Into<BlockId>,
        end_id: impl Into<BlockId>,
    ) -> Result<ReceiverStream<BlockScannerResult>, ScannerError> {
        let (blocks_sender, blocks_receiver) = mpsc::channel(MAX_BUFFERED_MESSAGES);
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::StreamHistorical {
            sender: blocks_sender,
            start_id: start_id.into(),
            end_id: end_id.into(),
            response: response_tx,
        };

        self.command_sender.send(command).await.map_err(|_| ScannerError::ServiceShutdown)?;

        response_rx.await.map_err(|_| ScannerError::ServiceShutdown)??;

        Ok(ReceiverStream::new(blocks_receiver))
    }

    /// Streams blocks starting from `start_id` and transitions to live mode.
    ///
    /// # Arguments
    ///
    /// * `start_id` - The starting block id.
    /// * `block_confirmations` - Number of confirmations to apply once in live mode.
    ///
    /// # Errors
    ///
    /// * `ScannerError::ServiceShutdown` - if the service is already shutting down.
    pub async fn stream_from(
        &self,
        start_id: impl Into<BlockId>,
        block_confirmations: u64,
    ) -> Result<ReceiverStream<BlockScannerResult>, ScannerError> {
        let (blocks_sender, blocks_receiver) = mpsc::channel(MAX_BUFFERED_MESSAGES);
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::StreamFrom {
            sender: blocks_sender,
            start_id: start_id.into(),
            block_confirmations,
            response: response_tx,
        };

        self.command_sender.send(command).await.map_err(|_| ScannerError::ServiceShutdown)?;

        response_rx.await.map_err(|_| ScannerError::ServiceShutdown)??;

        Ok(ReceiverStream::new(blocks_receiver))
    }

    /// Streams blocks in reverse order from `start_id` to `end_id`.
    ///
    /// # Arguments
    ///
    /// * `start_id` - The starting block id (defaults to Latest if None).
    /// * `end_id` - The ending block id (defaults to Earliest if None).
    ///
    /// # Errors
    ///
    /// * `ScannerError::ServiceShutdown` - if the service is already shutting down.
    pub async fn rewind(
        &self,
        start_id: impl Into<BlockId>,
        end_id: impl Into<BlockId>,
    ) -> Result<ReceiverStream<BlockScannerResult>, ScannerError> {
        let (blocks_sender, blocks_receiver) = mpsc::channel(MAX_BUFFERED_MESSAGES);
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::Rewind {
            sender: blocks_sender,
            start_id: start_id.into(),
            end_id: end_id.into(),
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
    use alloy::eips::{BlockId, BlockNumberOrTag};
    use tokio::sync::mpsc;

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
    async fn try_send_forwards_errors_to_subscribers() {
        let (tx, mut rx) = mpsc::channel::<BlockScannerResult>(1);

        _ = tx.try_stream(ScannerError::BlockNotFound(4.into())).await;

        assert!(matches!(
            rx.recv().await,
            Some(Err(ScannerError::BlockNotFound(BlockId::Number(BlockNumberOrTag::Number(4)))))
        ));
    }
}
