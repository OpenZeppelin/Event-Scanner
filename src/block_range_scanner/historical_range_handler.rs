use alloy::{
    eips::BlockNumberOrTag,
    network::{BlockResponse, Network, primitives::HeaderResponse},
    primitives::{BlockHash, BlockNumber},
};
use robust_provider::RobustProvider;
use tokio::sync::mpsc;

use crate::{
    Notification,
    block_range_scanner::{common::BlockScannerResult, range_iterator::RangeIterator},
    types::TryStream,
};

pub(crate) struct HistoricalRangeHandler<N: Network> {
    provider: RobustProvider<N>,
    max_block_range: u64,
    start: BlockNumber,
    end: BlockNumber,
    sender: mpsc::Sender<BlockScannerResult>,
}

impl<N: Network> HistoricalRangeHandler<N> {
    pub fn new(
        provider: RobustProvider<N>,
        max_block_range: u64,
        start: BlockNumber,
        end: BlockNumber,
        sender: mpsc::Sender<BlockScannerResult>,
    ) -> Self {
        Self { provider, max_block_range, start, end, sender }
    }

    pub fn run(self) {
        let HistoricalRangeHandler { provider, max_block_range, start, end, sender } = self;

        tokio::spawn(async move {
            let _ = Self::handle_stream_historical_range(
                start,
                end,
                max_block_range,
                &sender,
                &provider,
            )
            .await;
            debug!("Historical range stream ended");
        });
    }

    /// Public method for use by `sync_handler` during catchup phase.
    #[must_use]
    #[cfg_attr(feature = "tracing", tracing::instrument(level = "trace", skip(sender, provider)))]
    pub async fn stream_historical_range(
        start: BlockNumber,
        end: BlockNumber,
        max_block_range: u64,
        sender: &mpsc::Sender<BlockScannerResult>,
        provider: &RobustProvider<N>,
    ) -> Option<()> {
        Self::handle_stream_historical_range(start, end, max_block_range, sender, provider).await
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(level = "trace", skip(sender, provider)))]
    async fn handle_stream_historical_range(
        start: BlockNumber,
        end: BlockNumber,
        max_block_range: u64,
        sender: &mpsc::Sender<BlockScannerResult>,
        provider: &RobustProvider<N>,
    ) -> Option<()> {
        // Phase 1: Stream all finalized blocks without any reorg checks
        let non_finalized_start =
            Self::stream_finalized_blocks(provider, start, end, max_block_range, sender).await?;

        // All blocks already finalized
        if non_finalized_start > end {
            return Some(());
        }

        // Phase 2: Stream non-finalized blocks with reorg detection
        Self::stream_non_finalized_blocks(
            non_finalized_start,
            end,
            max_block_range,
            sender,
            provider,
        )
        .await
    }

    /// Streams finalized blocks without reorg checks.
    /// Returns the starting block number for non-finalized streaming, or None if channel closed.
    async fn stream_finalized_blocks(
        provider: &RobustProvider<N>,
        start: BlockNumber,
        end: BlockNumber,
        max_block_range: u64,
        sender: &mpsc::Sender<BlockScannerResult>,
    ) -> Option<BlockNumber> {
        // NOTE: Edge case - If the chain is too young to expose finalized blocks (height <
        // finalized depth) just use zero. Since we use the finalized block number only to
        // determine whether to run reorg checks or not, this is a "low-stakes" RPC call.
        let finalized_block_num =
            provider.get_block_number_by_id(BlockNumberOrTag::Finalized.into()).await.unwrap_or(0);

        let finalized_batch_end = finalized_block_num.min(end);

        for range in RangeIterator::forward(start, finalized_batch_end, max_block_range) {
            trace!(
                range_start = *range.start(),
                range_end = *range.end(),
                "Streaming finalized range"
            );
            if sender.try_stream(range).await.is_closed() {
                return None;
            }
        }

        // If start > finalized_batch_end, the loop above was empty and we should
        // continue from start. Otherwise, continue from after finalized_batch_end.
        Some(start.max(finalized_batch_end + 1))
    }

    /// Streams non-finalized blocks with reorg detection.
    /// Re-streams if a reorg is detected, repeating until stable.
    async fn stream_non_finalized_blocks(
        non_finalized_start: BlockNumber,
        end: BlockNumber,
        max_block_range: u64,
        sender: &mpsc::Sender<BlockScannerResult>,
        provider: &RobustProvider<N>,
    ) -> Option<()> {
        // Get the end block's hash before streaming
        let mut end_block_hash = match provider.get_block_by_number(end.into()).await {
            Ok(block) => block.header().hash(),
            Err(e) => {
                error!("Failed to get end block hash");
                _ = sender.try_stream(e).await;
                return None;
            }
        };

        loop {
            // Stream all non-finalized ranges
            for range in RangeIterator::forward(non_finalized_start, end, max_block_range) {
                trace!(
                    range_start = *range.start(),
                    range_end = *range.end(),
                    "Streaming non-finalized range"
                );
                if sender.try_stream(range).await.is_closed() {
                    return None;
                }
            }

            // Check for reorg - returns Some(new_hash) if reorg detected
            match Self::check_reorg(end, end_block_hash, non_finalized_start, sender, provider)
                .await
            {
                Some(new_hash) => end_block_hash = new_hash,
                None => return Some(()),
            }
        }
    }

    /// Checks if a reorg occurred by comparing block hashes.
    /// Returns `Some(new_hash)` if a reorg was detected (caller should re-stream),
    /// or `None` if no reorg (caller can finish) or if channel closed / error occurred.
    async fn check_reorg(
        end: BlockNumber,
        expected_hash: BlockHash,
        non_finalized_start: BlockNumber,
        sender: &mpsc::Sender<BlockScannerResult>,
        provider: &RobustProvider<N>,
    ) -> Option<BlockHash> {
        let current_end_block = match provider.get_block_by_number(end.into()).await {
            Ok(block) => block,
            Err(e) => {
                error!("Failed to fetch end block for reorg check");
                _ = sender.try_stream(e).await;
                return None;
            }
        };

        let current_hash = current_end_block.header().hash();
        if current_hash == expected_hash {
            debug!(end_block_hash = %expected_hash, "Historical sync completed");
            return None;
        }

        warn!(
            end_block = end,
            old_hash = %expected_hash,
            new_hash = %current_hash,
            "Reorg detected, re-streaming non-finalized blocks"
        );

        let common_ancestor = non_finalized_start.saturating_sub(1);
        if sender.try_stream(Notification::ReorgDetected { common_ancestor }).await.is_closed() {
            return None;
        }

        Some(current_hash)
    }
}
