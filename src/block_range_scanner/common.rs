use tokio::sync::mpsc;
use tokio_stream::StreamExt;

use crate::{
    ScannerError,
    block_range_scanner::{BlockScannerResult, reorg_handler::ReorgHandler},
    robust_provider::{RobustProvider, RobustSubscription, subscription},
    types::{Notification, TryStream},
};
use alloy::{
    consensus::BlockHeader,
    eips::BlockNumberOrTag,
    network::{BlockResponse, Network},
    primitives::BlockNumber,
};
use tracing::{debug, error, info, warn};

#[allow(clippy::too_many_arguments)]
pub(crate) async fn stream_live_blocks<N: Network>(
    stream_start: BlockNumber,
    subscription: RobustSubscription<N>,
    sender: &mpsc::Sender<BlockScannerResult>,
    provider: &RobustProvider<N>,
    block_confirmations: u64,
    max_block_range: u64,
    reorg_handler: &mut ReorgHandler<N>,
    notify_after_first_block: bool,
) {
    // Phase 1: Wait for first relevant block
    let mut stream =
        skip_to_first_relevant_block::<N>(subscription, stream_start, block_confirmations);

    let Some(first_block) = get_first_block::<N, _>(&mut stream, sender).await else {
        // error occurred and streamed
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

async fn get_first_block<
    N: Network,
    S: tokio_stream::Stream<Item = Result<N::HeaderResponse, subscription::Error>> + Unpin,
>(
    stream: &mut S,
    sender: &mpsc::Sender<BlockScannerResult>,
) -> Option<N::HeaderResponse> {
    while let Some(first_block) = stream.next().await {
        match first_block {
            Ok(block) => return Some(block),
            Err(e) => {
                match e {
                    subscription::Error::Lagged(_) => {
                        // scanner already accounts for skipped block numbers
                        // next block will be the actual incoming block
                        info!("Skipping Error::Lagged, next block should be the first live block");
                    }
                    subscription::Error::Timeout => {
                        _ = sender.try_stream(ScannerError::Timeout).await;
                        break;
                    }
                    subscription::Error::RpcError(rpc_err) => {
                        _ = sender.try_stream(ScannerError::RpcError(rpc_err)).await;
                        break;
                    }
                    subscription::Error::Closed => {
                        _ = sender.try_stream(ScannerError::SubscriptionClosed).await;
                        break;
                    }
                }
            }
        }
    }

    None
}

/// Skips blocks until we reach the first block that's relevant for streaming
fn skip_to_first_relevant_block<N: Network>(
    subscription: RobustSubscription<N>,
    stream_start: BlockNumber,
    block_confirmations: u64,
) -> impl tokio_stream::Stream<Item = Result<N::HeaderResponse, subscription::Error>> {
    subscription.into_stream().skip_while(move |header| match header {
        Ok(header) => header.number().saturating_sub(block_confirmations) < stream_start,
        Err(subscription::Error::Lagged(_)) => true,
        Err(_) => false,
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

    // The minimum common ancestor is the block before the stream start
    let min_common_ancestor = stream_start.saturating_sub(1);

    // Catch up on any confirmed blocks between stream_start and the confirmed tip
    let previous_batch_end = stream_range_with_reorg_handling(
        min_common_ancestor,
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
    S: tokio_stream::Stream<Item = Result<N::HeaderResponse, subscription::Error>> + Unpin,
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
        let incoming_block = match incoming_block {
            Ok(block) => block,
            Err(e) => {
                error!(error = %e, "Error receiving block from stream");
                match e {
                    subscription::Error::Lagged(_) => {
                        // scanner already accounts for skipped block numbers
                        // next block will be the actual incoming block
                        continue;
                    }
                    subscription::Error::Timeout => {
                        _ = sender.try_stream(ScannerError::Timeout).await;
                        return;
                    }
                    subscription::Error::RpcError(rpc_err) => {
                        _ = sender.try_stream(ScannerError::RpcError(rpc_err)).await;
                        return;
                    }
                    subscription::Error::Closed => {
                        _ = sender.try_stream(ScannerError::SubscriptionClosed).await;
                        return;
                    }
                }
            }
        };

        let incoming_block_num = incoming_block.number();
        info!(block_number = incoming_block_num, "Received block header");

        let Some(previous_batch_end) = state.previous_batch_end.as_ref() else {
            // previously detected reorg wasn't fully handled
            continue;
        };

        let common_ancestor = match reorg_handler.check(previous_batch_end).await {
            Ok(reorg_opt) => reorg_opt,
            Err(e) => {
                error!(error = %e, "Failed to perform reorg check");
                _ = sender.try_stream(e).await;
                return;
            }
        };

        if let Some(common_ancestor) = common_ancestor {
            if !handle_reorg_detected(common_ancestor, stream_start, state, sender).await {
                return; // Channel closed
            }
        } else {
            // No reorg: advance batch_start to after the previous batch
            state.batch_start = previous_batch_end.header().number() + 1;
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

    // The minimum common ancestor is the block before the stream start
    let min_common_ancestor = stream_start.saturating_sub(1);

    state.previous_batch_end = stream_range_with_reorg_handling(
        min_common_ancestor,
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

#[must_use]
pub(crate) async fn stream_historical_range<N: Network>(
    start: BlockNumber,
    end: BlockNumber,
    max_block_range: u64,
    sender: &mpsc::Sender<BlockScannerResult>,
    provider: &RobustProvider<N>,
    reorg_handler: &mut ReorgHandler<N>,
) -> Option<()> {
    info!("Getting finalized block number");
    let finalized = match provider.get_block_number_by_id(BlockNumberOrTag::Finalized.into()).await
    {
        Ok(block) => block,
        Err(e) => {
            error!(error = %e, "Failed to get finalized block");
            _ = sender.try_stream(e).await;
            return None;
        }
    };

    // no reorg check for finalized blocks
    let mut batch_start = start;
    let finalized_batch_end = finalized.min(end);
    while batch_start <= finalized_batch_end {
        let batch_end = batch_start.saturating_add(max_block_range - 1).min(finalized_batch_end);

        if !sender.try_stream(batch_start..=batch_end).await {
            return None; // channel closed
        }

        batch_start = batch_end + 1;
    }

    // covers case when `end <= finalized`
    if batch_start > end {
        return Some(()); // we're done
    }

    // we have non-finalized block numbers to stream, a reorg can occur

    // Possible minimal common ancestors when a reorg occurs:
    // * start > finalized -> the common ancestor we care about is the block before `start`, that's
    //   where the stream should restart -> this is why we used `start - 1`
    // * start == finalized -> `start` should never be re-streamed on reorgs; stream should restart
    //   on `start + 1`
    // * start < finalized -> if we got here, then `end > finalized`; on reorg, we should only
    //   re-stream non-finalized blocks
    let min_common_ancestor = (start.saturating_sub(1)).max(finalized);

    stream_range_with_reorg_handling(
        min_common_ancestor,
        batch_start,
        end,
        max_block_range,
        sender,
        provider,
        reorg_handler,
    )
    .await?;

    Some(())
}

/// Assumes that `min_common_ancestor <= next_start_block <= end`, performs no internal checks.
pub(crate) async fn stream_range_with_reorg_handling<N: Network>(
    min_common_ancestor: BlockNumber,
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
            return None; // channel closed
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

            min_common_ancestor.max(common_ancestor.header().number()) + 1
        } else {
            batch_end_num + 1
        };

        if next_start_block > end {
            info!(batch_count = batch_count, "Historical sync completed");
            return Some(batch_end);
        }
    }
}
