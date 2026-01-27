use alloy::{
    eips::BlockNumberOrTag,
    network::{BlockResponse, Network, primitives::HeaderResponse},
    primitives::BlockNumber,
};
use tokio::sync::mpsc;

use robust_provider::RobustProvider;

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

        info!(
            start_block = start,
            end_block = end,
            total_blocks = end.saturating_sub(start) + 1,
            "Starting historical range stream"
        );

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

    #[must_use]
    #[cfg_attr(feature = "tracing", tracing::instrument(level = "trace", skip(sender, provider)))]
    pub(crate) async fn stream_historical_range(
        start: BlockNumber,
        end: BlockNumber,
        max_block_range: u64,
        sender: &mpsc::Sender<BlockScannerResult>,
        provider: &RobustProvider<N>,
    ) -> Option<()> {
        Self::handle_stream_historical_range(start, end, max_block_range, sender, provider).await
    }

    #[must_use]
    #[allow(clippy::too_many_lines)]
    #[cfg_attr(feature = "tracing", tracing::instrument(level = "trace", skip(sender, provider)))]
    async fn handle_stream_historical_range(
        start: BlockNumber,
        end: BlockNumber,
        max_block_range: u64,
        sender: &mpsc::Sender<BlockScannerResult>,
        provider: &RobustProvider<N>,
    ) -> Option<()> {
        // NOTE: Edge case - If the chain is too young to expose finalized blocks (height <
        // finalized depth) just use zero.
        // Since we use the finalized block number only to determine whether to run reorg checks
        // or not, this is a "low-stakes" RPC call, for which, for simplicity, we can default to `0`
        // even on errors. Here `0` is used because it effectively just enables reorg checks.
        // If there was actually a provider problem, any subsequent provider call will catch and
        // properly log it and return the error to the caller.
        let finalized_block_num =
            provider.get_block_number_by_id(BlockNumberOrTag::Finalized.into()).await.unwrap_or(0);

        // Phase 1: Stream all finalized blocks without any reorg checks
        let finalized_batch_end = finalized_block_num.min(end);
        let finalized_range_count =
            RangeIterator::forward(start, finalized_batch_end, max_block_range).count();
        trace!(
            start = start,
            finalized_batch_end = finalized_batch_end,
            batch_count = finalized_range_count,
            "Streaming finalized blocks (no reorg check)"
        );

        for range in RangeIterator::forward(start, finalized_batch_end, max_block_range) {
            trace!(
                range_start = *range.start(),
                range_end = *range.end(),
                "Streaming finalized range"
            );
            if sender.try_stream(range).await.is_closed() {
                return None; // channel closed
            }
        }

        // If start > finalized_batch_end, the loop above was empty and we should
        // continue from start. Otherwise, continue from after finalized_batch_end.
        let batch_start = start.max(finalized_batch_end + 1);

        // covers case when `end <= finalized`
        if batch_start > end {
            return Some(()); // we're done
        }

        // Phase 2: Stream non-finalized blocks, then check for reorg only after the last range.
        // If a reorg occurred, re-stream all non-finalized blocks. Repeat until stable.
        let non_finalized_start = batch_start;

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
            // Stream all non-finalized ranges without intermediate reorg checks
            let non_finalized_range_count =
                RangeIterator::forward(non_finalized_start, end, max_block_range).count();
            trace!(
                non_finalized_start = non_finalized_start,
                end = end,
                batch_count = non_finalized_range_count,
                "Streaming non-finalized blocks (deferred reorg check)"
            );

            for range in RangeIterator::forward(non_finalized_start, end, max_block_range) {
                trace!(
                    range_start = *range.start(),
                    range_end = *range.end(),
                    "Streaming non-finalized range"
                );
                if sender.try_stream(range).await.is_closed() {
                    return None; // channel closed
                }
            }

            // After streaming, fetch the current canonical block and compare hashes (reorg check)
            let current_end_block = match provider.get_block_by_number(end.into()).await {
                Ok(block) => block,
                Err(e) => {
                    error!("Failed to fetch end block for reorg check");
                    _ = sender.try_stream(e).await;
                    return None;
                }
            };

            let current_hash = current_end_block.header().hash();
            if current_hash == end_block_hash {
                // Same hash - no reorg, we're done
                debug!(
                    end_block_hash = %end_block_hash,
                    "Historical sync completed, end block hash verified"
                );
                return Some(());
            }

            // Different hash - reorg detected
            warn!(
                end_block = end,
                old_hash = %end_block_hash,
                new_hash = %current_hash,
                "Reorg detected after streaming last range, re-streaming non-finalized blocks"
            );

            // For historic mode, using `non_finalized_start - 1` as a reasonable estimate for
            // common_ancestor.
            let common_ancestor = non_finalized_start.saturating_sub(1);
            if sender.try_stream(Notification::ReorgDetected { common_ancestor }).await.is_closed()
            {
                return None; // channel closed
            }

            // Update to the new canonical hash for the next iteration
            end_block_hash = current_hash;

            // Check if finalized has advanced past end (all blocks now finalized)
            let current_finalized =
                match provider.get_block_number_by_id(BlockNumberOrTag::Finalized.into()).await {
                    Ok(block) => block,
                    Err(e) => {
                        error!("Failed to get updated finalized block");
                        _ = sender.try_stream(e).await;
                        return None;
                    }
                };

            if current_finalized >= end {
                debug!(
                    finalized = current_finalized,
                    end = end,
                    "End block is now finalized, historical sync completed"
                );
                return Some(());
            }
        }
    }
}
