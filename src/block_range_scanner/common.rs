use tokio::sync::mpsc;
use tokio_stream::StreamExt;

use crate::{
    block_range_scanner::{BlockScannerResult, reorg_handler::ReorgHandler},
    robust_provider::RobustProvider,
    types::{Notification, TryStream},
};
use alloy::{
    consensus::BlockHeader,
    network::{BlockResponse, Network},
    primitives::BlockNumber,
    pubsub::Subscription,
};
use tracing::{debug, error, info, warn};

#[allow(clippy::too_many_arguments)]
pub(crate) async fn stream_live_blocks<N: Network>(
    stream_start: BlockNumber,
    subscription: Subscription<N::HeaderResponse>,
    sender: &mpsc::Sender<BlockScannerResult>,
    provider: &RobustProvider<N>,
    block_confirmations: u64,
    max_block_range: u64,
    reorg_handler: &mut ReorgHandler<N>,
    notify_after_first_block: bool,
) {
    // Phase 1: Wait for first relevant block
    let mut stream = skip_to_relevant_blocks::<N>(subscription, stream_start, block_confirmations);

    let Some(first_block) = stream.next().await else {
        warn!("Subscription channel closed before receiving any blocks");
        return;
    };

    if notify_after_first_block && !sender.try_stream(Notification::SwitchingToLive).await {
        return;
    }

    // Phase 2: Initialize streaming state with first block
    let Some(mut state) = initialize_live_streaming_state(
        first_block,
        stream_start,
        block_confirmations,
        max_block_range,
        sender,
        provider,
        reorg_handler,
    )
    .await
    else {
        return;
    };

    // Phase 3: Continuously stream blocks with reorg handling
    stream_blocks_continuously(
        &mut stream,
        &mut state,
        stream_start,
        block_confirmations,
        max_block_range,
        sender,
        provider,
        reorg_handler,
    )
    .await;

    warn!("Live block subscription ended");
}

/// Skips blocks until we reach the first block that's relevant for streaming
fn skip_to_relevant_blocks<N: Network>(
    subscription: Subscription<N::HeaderResponse>,
    stream_start: BlockNumber,
    block_confirmations: u64,
) -> impl tokio_stream::Stream<Item = N::HeaderResponse> {
    subscription.into_stream().skip_while(move |header| {
        header.number().saturating_sub(block_confirmations) < stream_start
    })
}

/// Initializes the streaming state after receiving the first block
/// Returns None if the channel is closed
async fn initialize_live_streaming_state<N: Network>(
    first_block: N::HeaderResponse,
    stream_start: BlockNumber,
    block_confirmations: u64,
    max_block_range: u64,
    sender: &mpsc::Sender<BlockScannerResult>,
    provider: &RobustProvider<N>,
    reorg_handler: &mut ReorgHandler<N>,
) -> Option<LiveStreamingState<N>> {
    let incoming_block_num = first_block.number();
    info!(block_number = incoming_block_num, "Received first block header");

    let confirmed = incoming_block_num.saturating_sub(block_confirmations);

    // Catch up on any confirmed blocks between stream_start and the confirmed tip
    let previous_batch_end = stream_block_range(
        stream_start,
        stream_start,
        confirmed,
        max_block_range,
        sender,
        provider,
        reorg_handler,
    )
    .await?;

    Some(LiveStreamingState {
        batch_start: stream_start,
        previous_batch_end: Some(previous_batch_end),
    })
}

/// Continuously streams blocks, handling reorgs as they occur
#[allow(clippy::too_many_arguments)]
async fn stream_blocks_continuously<
    N: Network,
    S: tokio_stream::Stream<Item = N::HeaderResponse> + Unpin,
>(
    stream: &mut S,
    state: &mut LiveStreamingState<N>,
    stream_start: BlockNumber,
    block_confirmations: u64,
    max_block_range: u64,
    sender: &mpsc::Sender<BlockScannerResult>,
    provider: &RobustProvider<N>,
    reorg_handler: &mut ReorgHandler<N>,
) {
    while let Some(incoming_block) = stream.next().await {
        let incoming_block_num = incoming_block.number();
        info!(block_number = incoming_block_num, "Received block header");

        // Check for reorgs and update state accordingly
        let Some(common_ancestor) =
            check_for_reorg(state.previous_batch_end.as_ref(), reorg_handler, sender).await
        else {
            return;
        };

        if let Some(common_ancestor) = common_ancestor {
            if !handle_reorg_detected(common_ancestor, stream_start, state, sender).await {
                return; // Channel closed
            }
        } else {
            // No reorg: advance batch_start to after the previous batch
            advance_batch_start_after_previous_end(state);
        }

        // Stream the next batch of confirmed blocks
        let batch_end_num = incoming_block_num.saturating_sub(block_confirmations);
        if !stream_next_batch(
            batch_end_num,
            state,
            stream_start,
            max_block_range,
            sender,
            provider,
            reorg_handler,
        )
        .await
        {
            return; // Channel closed
        }
    }
}

/// Checks if a reorg occurred by verifying the previous batch end block.
/// Returns `None` if the channel is closed.
async fn check_for_reorg<N: Network>(
    previous_batch_end: Option<&N::BlockResponse>,
    reorg_handler: &mut ReorgHandler<N>,
    sender: &mpsc::Sender<BlockScannerResult>,
) -> Option<Option<N::BlockResponse>> {
    let batch_end = previous_batch_end?;

    match reorg_handler.check(batch_end).await {
        Ok(reorg_opt) => Some(reorg_opt),
        Err(e) => {
            error!(error = %e, "Failed to perform reorg check");
            _ = sender.try_stream(e).await;
            None
        }
    }
}

/// Handles a detected reorg by notifying and adjusting the streaming state
/// Returns false if the channel is closed
async fn handle_reorg_detected<N: Network>(
    common_ancestor: N::BlockResponse,
    stream_start: BlockNumber,
    state: &mut LiveStreamingState<N>,
    sender: &mpsc::Sender<BlockScannerResult>,
) -> bool {
    if !sender.try_stream(Notification::ReorgDetected).await {
        return false;
    }

    let ancestor_num = common_ancestor.header().number();

    // Reset streaming position based on common ancestor
    if ancestor_num < stream_start {
        // Reorg went before our starting point - restart from stream_start
        info!(
            ancestor_block = ancestor_num,
            stream_start = stream_start,
            "Reorg detected before stream start, resetting to stream start"
        );
        state.batch_start = stream_start;
        state.previous_batch_end = None;
    } else {
        // Resume from after the common ancestor
        info!(ancestor_block = ancestor_num, "Reorg detected, resuming from common ancestor");
        state.batch_start = ancestor_num + 1;
        state.previous_batch_end = Some(common_ancestor);
    }

    true
}

/// Advances `batch_start` after processing a normal (non-reorg) block
fn advance_batch_start_after_previous_end<N: Network>(state: &mut LiveStreamingState<N>) {
    if let Some(prev_batch_end) = state.previous_batch_end.as_ref() {
        state.batch_start = prev_batch_end.header().number() + 1;
    }
}

/// Streams the next batch of blocks up to `batch_end_num`.
/// Returns false if the channel is closed
async fn stream_next_batch<N: Network>(
    batch_end_num: BlockNumber,
    state: &mut LiveStreamingState<N>,
    stream_start: BlockNumber,
    max_block_range: u64,
    sender: &mpsc::Sender<BlockScannerResult>,
    provider: &RobustProvider<N>,
    reorg_handler: &mut ReorgHandler<N>,
) -> bool {
    if batch_end_num < state.batch_start {
        // No new confirmed blocks to stream yet
        return true;
    }

    state.previous_batch_end = stream_block_range(
        stream_start,
        state.batch_start,
        batch_end_num,
        max_block_range,
        sender,
        provider,
        reorg_handler,
    )
    .await;

    if state.previous_batch_end.is_none() {
        // Channel closed
        return false;
    }

    // SAFETY: Overflow cannot realistically happen
    state.batch_start = batch_end_num + 1;

    true
}

/// Tracks the current state of live streaming
struct LiveStreamingState<N: Network> {
    /// The starting block number for the next batch to stream
    batch_start: BlockNumber,
    /// The last block from the previous batch (used for reorg detection)
    previous_batch_end: Option<N::BlockResponse>,
}

/// Assumes that `min_block <= next_start_block <= end`.
pub(crate) async fn stream_block_range<N: Network>(
    min_block: BlockNumber,
    mut next_start_block: BlockNumber,
    end: BlockNumber,
    max_block_range: u64,
    sender: &mpsc::Sender<BlockScannerResult>,
    provider: &RobustProvider<N>,
    reorg_handler: &mut ReorgHandler<N>,
) -> Option<N::BlockResponse> {
    let mut batch_count = 0;

    loop {
        let batch_end_num = next_start_block.saturating_add(max_block_range - 1).min(end);
        let batch_end = match provider.get_block_by_number(batch_end_num.into()).await {
            Ok(block) => block,
            Err(e) => {
                error!(batch_start = next_start_block, batch_end = batch_end_num, error = %e, "Failed to get ending block of the current batch");
                _ = sender.try_stream(e).await;
                return None;
            }
        };

        if !sender.try_stream(next_start_block..=batch_end_num).await {
            return Some(batch_end);
        }

        batch_count += 1;
        if batch_count % 10 == 0 {
            debug!(batch_count = batch_count, "Processed historical batches");
        }

        let reorged_opt = match reorg_handler.check(&batch_end).await {
            Ok(opt) => opt,
            Err(e) => {
                error!(error = %e, "Failed to perform reorg check");
                _ = sender.try_stream(e).await;
                return None;
            }
        };

        next_start_block = if let Some(common_ancestor) = reorged_opt {
            if !sender.try_stream(Notification::ReorgDetected).await {
                return None;
            }
            if common_ancestor.header().number() < min_block {
                min_block
            } else {
                common_ancestor.header().number() + 1
            }
        } else {
            batch_end_num.saturating_add(1)
        };

        if next_start_block > end {
            info!(batch_count = batch_count, "Historical sync completed");
            return Some(batch_end);
        }
    }
}
