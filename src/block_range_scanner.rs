//! Block-range streaming service.
//!
//! This module provides a lower-level primitive used by [`crate::EventScanner`]: it streams
//! contiguous block number ranges (inclusive) and emits [`crate::Notification`] values for
//! certain state transitions (e.g. reorg detection).
//!
//! [`BlockRangeScanner`] is useful when you want to build your own log-fetching pipeline on top of
//! range streaming, or when you need direct access to the scanner's batching and reorg-detection
//! behavior.
//!
//! # Output stream
//!
//! Streams returned by [`ConnectedBlockRangeScanner`] yield [`BlockScannerResult`] items:
//!
//! - `Ok(ScannerMessage::Data(range))` for a block range to process.
//! - `Ok(ScannerMessage::Notification(_))` for scanner notifications.
//! - `Err(ScannerError)` for errors.
//!
//! # Ordering
//!
//! Range messages are streamed in chronological order within a single stream (lower block number
//! to higher block number). On reorgs, the scanner may re-emit previously-seen ranges for the
//! affected blocks.
//!
//! # Example usage:
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
//!         BlockRangeScanner, DEFAULT_BLOCK_CONFIRMATIONS, DEFAULT_MAX_BLOCK_RANGE,
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
//!     let mut stream = block_range_scanner
//!         .stream_from(BlockNumberOrTag::Number(5), DEFAULT_BLOCK_CONFIRMATIONS)
//!         .await?;
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
//!                     ScannerError::Lagged(_) => break,
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
use tokio::{sync::mpsc, try_join};
use tokio_stream::wrappers::ReceiverStream;

use crate::{
    ScannerError, ScannerMessage,
    block_range_scanner::sync_handler::SyncHandler,
    robust_provider::{IntoRobustProvider, RobustProvider},
    types::{IntoScannerResult, Notification, ScannerResult, TryStream},
};

use alloy::{
    consensus::BlockHeader,
    eips::{BlockId, BlockNumberOrTag},
    network::{BlockResponse, Network},
    primitives::BlockNumber,
};

mod common;
mod range_iterator;
mod reorg_handler;
mod ring_buffer;
mod sync_handler;

pub(crate) use range_iterator::RangeIterator;

use reorg_handler::ReorgHandler;
pub use ring_buffer::RingBufferCapacity;

/// Default maximum number of blocks per streamed range.
pub const DEFAULT_MAX_BLOCK_RANGE: u64 = 1000;

/// Default confirmation depth used by scanners that accept a `block_confirmations` setting.
pub const DEFAULT_BLOCK_CONFIRMATIONS: u64 = 0;

/// Default per-stream buffer size used by scanners.
pub const DEFAULT_STREAM_BUFFER_CAPACITY: usize = 50000;

/// The result type yielded by block-range streams.
pub type BlockScannerResult = ScannerResult<RangeInclusive<BlockNumber>>;

/// Convenience alias for a streamed block-range message.
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

/// Builder/configuration for the block-range streaming service.
#[derive(Clone, Debug)]
pub struct BlockRangeScanner {
    /// Maximum number of blocks per streamed range.
    pub max_block_range: u64,
    /// How many past block hashes to keep in memory for reorg detection.
    ///
    /// If set to `RingBufferCapacity::Limited(0)`, reorg detection is disabled.
    pub past_blocks_storage_capacity: RingBufferCapacity,
    pub buffer_capacity: usize,
}

impl Default for BlockRangeScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockRangeScanner {
    /// Creates a scanner with default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_block_range: DEFAULT_MAX_BLOCK_RANGE,
            past_blocks_storage_capacity: RingBufferCapacity::Limited(10),
            buffer_capacity: DEFAULT_STREAM_BUFFER_CAPACITY,
        }
    }

    /// Sets the maximum number of blocks per streamed range.
    ///
    /// This controls batching for historical scans and for catch-up in live/sync scanners.
    ///
    /// Must be greater than 0.
    #[must_use]
    pub fn max_block_range(mut self, max_block_range: u64) -> Self {
        self.max_block_range = max_block_range;
        self
    }

    /// Sets how many past block hashes to keep in memory for reorg detection.
    ///
    /// If set to `RingBufferCapacity::Limited(0)`, reorg detection is disabled.
    #[must_use]
    pub fn past_blocks_storage_capacity(
        mut self,
        past_blocks_storage_capacity: RingBufferCapacity,
    ) -> Self {
        self.past_blocks_storage_capacity = past_blocks_storage_capacity;
        self
    }

    /// Sets the stream buffer capacity.
    ///
    /// Controls the maximum number of messages that can be buffered in the stream
    /// before backpressure is applied.
    ///
    /// # Arguments
    ///
    /// * `buffer_capacity` - Maximum number of messages to buffer (must be greater than 0)
    #[must_use]
    pub fn buffer_capacity(mut self, buffer_capacity: usize) -> Self {
        self.buffer_capacity = buffer_capacity;
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
        if self.max_block_range == 0 {
            return Err(ScannerError::InvalidMaxBlockRange);
        }
        if self.buffer_capacity == 0 {
            return Err(ScannerError::InvalidBufferCapacity);
        }
        let provider = provider.into_robust_provider().await?;
        Ok(ConnectedBlockRangeScanner {
            provider,
            max_block_range: self.max_block_range,
            past_blocks_storage_capacity: self.past_blocks_storage_capacity,
            buffer_capacity: self.buffer_capacity,
        })
    }
}

/// A [`BlockRangeScanner`] connected to a provider.
#[derive(Debug)]
pub struct ConnectedBlockRangeScanner<N: Network> {
    provider: RobustProvider<N>,
    max_block_range: u64,
    past_blocks_storage_capacity: RingBufferCapacity,
    buffer_capacity: usize,
}

impl<N: Network> ConnectedBlockRangeScanner<N> {
    /// Returns the underlying [`RobustProvider`].
    #[must_use]
    pub fn provider(&self) -> &RobustProvider<N> {
        &self.provider
    }

    /// Returns the stream buffer capacity.
    #[must_use]
    pub fn buffer_capacity(&self) -> usize {
        self.buffer_capacity
    }

    /// Streams live blocks starting from the latest block.
    ///
    /// # Arguments
    ///
    /// * `block_confirmations` - Number of confirmations to apply once in live mode.
    ///
    /// # Errors
    ///
    /// * [`ScannerError::Timeout`] - if an RPC call required for startup times out.
    /// * [`ScannerError::RpcError`] - if an RPC call required for startup fails.
    pub async fn stream_live(
        &mut self,
        block_confirmations: u64,
    ) -> Result<ReceiverStream<BlockScannerResult>, ScannerError> {
        info!("Starting live stream");
        let (blocks_sender, blocks_receiver) = mpsc::channel(self.buffer_capacity);
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
                &blocks_sender,
                &provider,
                block_confirmations,
                max_block_range,
                &mut reorg_handler,
                false, // (notification unnecessary)
            )
            .await;
        });

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
    /// * [`ScannerError::Timeout`] - if an RPC call required for startup times out.
    /// * [`ScannerError::RpcError`] - if an RPC call required for startup fails.
    /// * [`ScannerError::BlockNotFound`] - if `start_id` or `end_id` cannot be resolved.
    pub async fn stream_historical(
        &mut self,
        start_id: impl Into<BlockId>,
        end_id: impl Into<BlockId>,
    ) -> Result<ReceiverStream<BlockScannerResult>, ScannerError> {
        let start_id = start_id.into();
        let end_id = end_id.into();
        info!(start_id = ?start_id, end_id = ?end_id, "Starting historical stream");
        let (blocks_sender, blocks_receiver) = mpsc::channel(self.buffer_capacity);

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

        info!(
            start_block = start_block_num,
            end_block = end_block_num,
            "Normalized the block range"
        );

        tokio::spawn(async move {
            let mut reorg_handler =
                ReorgHandler::new(provider.clone(), past_blocks_storage_capacity);

            _ = common::stream_historical_range(
                start_block_num,
                end_block_num,
                max_block_range,
                &blocks_sender,
                &provider,
                &mut reorg_handler,
            )
            .await;
        });

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
    /// * [`ScannerError::Timeout`] - if an RPC call required for startup times out.
    /// * [`ScannerError::RpcError`] - if an RPC call required for startup fails.
    /// * [`ScannerError::BlockNotFound`] - if `start_id` cannot be resolved.
    pub async fn stream_from(
        &self,
        start_id: impl Into<BlockId>,
        block_confirmations: u64,
    ) -> Result<ReceiverStream<BlockScannerResult>, ScannerError> {
        let start_id = start_id.into();
        info!(start_id = ?start_id, "Starting streaming from");

        let (blocks_sender, blocks_receiver) = mpsc::channel(self.buffer_capacity);
        let sync_handler = SyncHandler::new(
            self.provider.clone(),
            self.max_block_range,
            start_id,
            block_confirmations,
            self.past_blocks_storage_capacity,
            blocks_sender,
        );

        sync_handler.run().await?;

        Ok(ReceiverStream::new(blocks_receiver))
    }

    /// Streams blocks in reverse order from `start_id` to `end_id`.
    ///
    /// The `start_id` block is assumed to be greater than or equal to the `end_id` block.
    /// Blocks are streamed in batches, where each batch is ordered from lower to higher
    /// block numbers (chronological order within each batch), but batches themselves
    /// progress from newer to older blocks.
    ///
    /// # Arguments
    ///
    /// * `start_id` - The starting block id (higher block number).
    /// * `end_id` - The ending block id (lower block number).
    ///
    /// # Reorg Handling
    ///
    /// Reorg checks are only performed when the specified block range tip is above the
    /// current finalized block height. When a reorg is detected:
    ///
    /// 1. A [`Notification::ReorgDetected`] is emitted with the common ancestor block
    /// 2. The scanner fetches the new tip block at the same height
    /// 3. Reorged blocks are re-streamed in chronological order (from `common_ancestor + 1` up to
    ///    the new tip)
    /// 4. The reverse scan continues from where it left off
    ///
    /// If the range tip is at or below the finalized block, no reorg checks are
    /// performed since finalized blocks cannot be reorganized.
    ///
    /// # Note
    ///
    /// The reason reorged blocks are streamed in chronological order is to make it easier to handle
    /// reorgs in [`EventScannerBuilder::latest`][latest mode] mode, i.e. to prepend reorged blocks
    /// to the result collection, which must maintain chronological order.
    ///
    /// [latest mode]: crate::EventScannerBuilder::latest
    ///
    /// # Errors
    ///
    /// * [`ScannerError::Timeout`] - if an RPC call required for startup times out.
    /// * [`ScannerError::RpcError`] - if an RPC call required for startup fails.
    /// * [`ScannerError::BlockNotFound`] - if `start_id` or `end_id` cannot be resolved.
    pub async fn stream_rewind(
        &mut self,
        start_id: impl Into<BlockId>,
        end_id: impl Into<BlockId>,
    ) -> Result<ReceiverStream<BlockScannerResult>, ScannerError> {
        let start_id = start_id.into();
        let end_id = end_id.into();
        info!(start_id = ?start_id, end_id = ?end_id, "Starting rewind");
        let (blocks_sender, blocks_receiver) = mpsc::channel(self.buffer_capacity);
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

            Self::handle_stream_rewind(
                from,
                to,
                max_block_range,
                &blocks_sender,
                &provider,
                &mut reorg_handler,
            )
            .await;
        });

        Ok(ReceiverStream::new(blocks_receiver))
    }

    /// Streams blocks in reverse order from `from` to `to`.
    async fn handle_stream_rewind(
        from: N::BlockResponse,
        to: N::BlockResponse,
        max_block_range: u64,
        sender: &mpsc::Sender<BlockScannerResult>,
        provider: &RobustProvider<N>,
        reorg_handler: &mut ReorgHandler<N>,
    ) {
        // for checking whether reorg occurred
        let mut tip = from;

        let from = tip.header().number();
        let to = to.header().number();

        let finalized_block = match provider.get_block_by_number(BlockNumberOrTag::Finalized).await
        {
            Ok(block) => block,
            Err(e) => {
                error!(error = %e, "Failed to get finalized block");
                _ = sender.try_stream(e).await;
                return;
            }
        };

        let finalized_number = finalized_block.header().number();

        // only check reorg if our tip is after the finalized block
        let check_reorg = tip.header().number() > finalized_number;

        let mut iter = RangeIterator::reverse(from, to, max_block_range);
        for range in &mut iter {
            // stream the range regularly, i.e. from smaller block number to greater
            if !sender.try_stream(range).await {
                break;
            }

            if check_reorg {
                let reorg = match reorg_handler.check(&tip).await {
                    Ok(opt) => opt,
                    Err(e) => {
                        error!(error = %e, "Terminal RPC call error, shutting down");
                        _ = sender.try_stream(e).await;
                        return;
                    }
                };

                if let Some(common_ancestor) = reorg &&
                    !Self::handle_reorg_rescan(
                        &mut tip,
                        common_ancestor,
                        max_block_range,
                        sender,
                        provider,
                    )
                    .await
                {
                    return;
                }
            }
        }

        info!(batch_count = iter.batch_count(), "Rewind completed");
    }

    /// Handles re-scanning of reorged blocks.
    ///
    /// Returns `true` on success, `false` if stream closed or terminal error occurred.
    async fn handle_reorg_rescan(
        tip: &mut N::BlockResponse,
        common_ancestor: N::BlockResponse,
        max_block_range: u64,
        sender: &mpsc::Sender<BlockScannerResult>,
        provider: &RobustProvider<N>,
    ) -> bool {
        let tip_number = tip.header().number();
        let common_ancestor = common_ancestor.header().number();
        info!(
            block_number = %tip_number,
            hash = %alloy::network::primitives::HeaderResponse::hash(tip.header()),
            common_ancestor = %common_ancestor,
            "Reorg detected"
        );

        if !sender.try_stream(Notification::ReorgDetected { common_ancestor }).await {
            return false;
        }

        // Get the new tip block (same height as original tip, but new hash)
        *tip = match provider.get_block_by_number(tip_number.into()).await {
            Ok(block) => block,
            Err(e) => {
                if matches!(e, crate::robust_provider::Error::BlockNotFound(_)) {
                    error!("Unexpected error: pre-reorg chain tip should exist on a reorged chain");
                } else {
                    error!(error = %e, "Terminal RPC call error, shutting down");
                }
                _ = sender.try_stream(e).await;
                return false;
            }
        };

        // Re-scan only the affected range (from common_ancestor + 1 up to tip)
        let rescan_from = common_ancestor + 1;

        for batch in RangeIterator::forward(rescan_from, tip_number, max_block_range) {
            if !sender.try_stream(batch).await {
                return false;
            }
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::{
        eips::{BlockId, BlockNumberOrTag},
        network::Ethereum,
        providers::{RootProvider, mock::Asserter},
        rpc::client::RpcClient,
    };
    use tokio::sync::mpsc;

    #[test]
    fn block_range_scanner_defaults_match_constants() {
        let scanner = BlockRangeScanner::new();

        assert_eq!(scanner.max_block_range, DEFAULT_MAX_BLOCK_RANGE);
        assert_eq!(scanner.buffer_capacity, DEFAULT_STREAM_BUFFER_CAPACITY);
    }

    #[test]
    fn builder_methods_update_configuration() {
        let scanner = BlockRangeScanner::new().max_block_range(42).buffer_capacity(33);

        assert_eq!(scanner.max_block_range, 42);
        assert_eq!(scanner.buffer_capacity, 33);
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

    #[tokio::test]
    async fn returns_error_with_zero_buffer_capacity() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let result = BlockRangeScanner::new().buffer_capacity(0).connect(provider).await;

        assert!(matches!(result, Err(ScannerError::InvalidBufferCapacity)));
    }

    #[tokio::test]
    async fn returns_error_with_zero_max_block_range() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let result = BlockRangeScanner::new().max_block_range(0).connect(provider).await;

        assert!(matches!(result, Err(ScannerError::InvalidMaxBlockRange)));
    }
}
