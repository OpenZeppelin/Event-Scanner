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
//!     BlockRangeScannerBuilder, DEFAULT_BLOCK_CONFIRMATIONS, ScannerError, ScannerMessage,
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
//!     let block_range_scanner = BlockRangeScannerBuilder::new().connect(robust_provider).await?;
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

use std::{cmp::Ordering, fmt::Debug};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::{
    ScannerError,
    block_range_scanner::{
        RingBufferCapacity,
        common::{self, BlockScannerResult},
        reorg_handler::ReorgHandler,
        rewind_handler::RewindHandler,
        sync_handler::SyncHandler,
    },
    robust_provider::RobustProvider,
};

use alloy::{
    consensus::BlockHeader,
    eips::BlockId,
    network::{BlockResponse, Network},
};

/// A [`BlockRangeScanner`] connected to a provider.
#[derive(Debug)]
pub struct BlockRangeScanner<N: Network> {
    pub(crate) provider: RobustProvider<N>,
    pub(crate) max_block_range: u64,
    pub(crate) past_blocks_storage_capacity: RingBufferCapacity,
    pub(crate) buffer_capacity: usize,
}

impl<N: Network> BlockRangeScanner<N> {
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
    #[cfg_attr(feature = "tracing", tracing::instrument(level = "trace", skip_all))]
    pub async fn stream_live(
        &self,
        block_confirmations: u64,
    ) -> Result<ReceiverStream<BlockScannerResult>, ScannerError> {
        let (blocks_sender, blocks_receiver) = mpsc::channel(self.buffer_capacity);

        let max_block_range = self.max_block_range;
        let past_blocks_storage_capacity = self.past_blocks_storage_capacity;
        let latest = self.provider.get_block_number().await?;
        let provider = self.provider.clone();

        // the next block returned by the underlying subscription will always be `latest + 1`,
        // because `latest` was already mined and subscription by definition only streams after new
        // blocks have been mined
        let start_block = (latest + 1).saturating_sub(block_confirmations);

        debug!(
            latest_block = latest,
            start_block = start_block,
            block_confirmations = block_confirmations,
            max_block_range = max_block_range,
            "Starting live block stream"
        );

        let subscription = self.provider.subscribe_blocks().await?;

        tokio::spawn(async move {
            let mut reorg_handler =
                ReorgHandler::new(provider.clone(), past_blocks_storage_capacity);

            common::stream_live_blocks(
                start_block,
                subscription,
                &blocks_sender,
                &provider,
                block_confirmations,
                max_block_range,
                &mut reorg_handler,
                false, // (notification unnecessary)
            )
            .await;

            debug!("Live block stream ended");
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
    #[cfg_attr(feature = "tracing", tracing::instrument(level = "trace", skip_all))]
    pub async fn stream_historical(
        &self,
        start_id: impl Into<BlockId>,
        end_id: impl Into<BlockId>,
    ) -> Result<ReceiverStream<BlockScannerResult>, ScannerError> {
        let (blocks_sender, blocks_receiver) = mpsc::channel(self.buffer_capacity);

        let max_block_range = self.max_block_range;
        let past_blocks_storage_capacity = self.past_blocks_storage_capacity;
        let provider = self.provider.clone();

        let (start_block, end_block) = tokio::try_join!(
            self.provider.get_block(start_id.into()),
            self.provider.get_block(end_id.into())
        )?;

        let start_block_num = start_block.header().number();
        let end_block_num = end_block.header().number();

        let (start_block_num, end_block_num) = match start_block_num.cmp(&end_block_num) {
            Ordering::Greater => (end_block_num, start_block_num),
            _ => (start_block_num, end_block_num),
        };

        let total_blocks = end_block_num.saturating_sub(start_block_num) + 1;
        debug!(
            from_block = start_block_num,
            to_block = end_block_num,
            total_blocks = total_blocks,
            max_block_range = max_block_range,
            "Starting historical block stream"
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

            debug!("Historical block stream completed");
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
    #[cfg_attr(feature = "tracing", tracing::instrument(level = "trace", skip_all))]
    pub async fn stream_from(
        &self,
        start_id: impl Into<BlockId>,
        block_confirmations: u64,
    ) -> Result<ReceiverStream<BlockScannerResult>, ScannerError> {
        let (blocks_sender, blocks_receiver) = mpsc::channel(self.buffer_capacity);

        let start_id = start_id.into();
        debug!(
            start_block = ?start_id,
            block_confirmations = block_confirmations,
            max_block_range = self.max_block_range,
            "Starting sync block stream"
        );

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
    /// 1. A [`Notification::ReorgDetected`][reorg] is emitted with the common ancestor block
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
    /// # Errors
    ///
    /// * [`ScannerError::Timeout`] - if an RPC call required for startup times out.
    /// * [`ScannerError::RpcError`] - if an RPC call required for startup fails.
    /// * [`ScannerError::BlockNotFound`] - if `start_id` or `end_id` cannot be resolved.
    ///
    /// [latest mode]: crate::EventScannerBuilder::latest
    /// [reorg]: crate::Notification::ReorgDetected
    #[cfg_attr(feature = "tracing", tracing::instrument(level = "trace", skip_all))]
    pub async fn stream_rewind(
        &self,
        start_id: impl Into<BlockId>,
        end_id: impl Into<BlockId>,
    ) -> Result<ReceiverStream<BlockScannerResult>, ScannerError> {
        let (blocks_sender, blocks_receiver) = mpsc::channel(self.buffer_capacity);

        let start_id = start_id.into();
        let end_id = end_id.into();
        debug!(
            from_block = ?start_id,
            to_block = ?end_id,
            max_block_range = self.max_block_range,
            "Starting rewind block stream"
        );

        let rewind_handler = RewindHandler::new(
            self.provider.clone(),
            self.max_block_range,
            start_id,
            end_id,
            self.past_blocks_storage_capacity,
            blocks_sender,
        );

        rewind_handler.run().await?;

        Ok(ReceiverStream::new(blocks_receiver))
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        block_range_scanner::{
            BlockRangeScannerBuilder, DEFAULT_MAX_BLOCK_RANGE, DEFAULT_STREAM_BUFFER_CAPACITY,
        },
        types::TryStream,
    };

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
        let scanner = BlockRangeScannerBuilder::new();

        assert_eq!(scanner.max_block_range, DEFAULT_MAX_BLOCK_RANGE);
        assert_eq!(scanner.buffer_capacity, DEFAULT_STREAM_BUFFER_CAPACITY);
    }

    #[test]
    fn builder_methods_update_configuration() {
        let scanner = BlockRangeScannerBuilder::new().max_block_range(42).buffer_capacity(33);

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
        let result = BlockRangeScannerBuilder::new().buffer_capacity(0).connect(provider).await;

        assert!(matches!(result, Err(ScannerError::InvalidBufferCapacity)));
    }

    #[tokio::test]
    async fn returns_error_with_zero_max_block_range() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let result = BlockRangeScannerBuilder::new().max_block_range(0).connect(provider).await;

        assert!(matches!(result, Err(ScannerError::InvalidMaxBlockRange)));
    }
}
