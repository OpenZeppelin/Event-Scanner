use std::ops::RangeInclusive;

use robust_provider::{RobustProvider, RobustSubscription, subscription};
use tokio::sync::mpsc;
use tokio_stream::StreamExt;

use crate::{
    ScannerError, ScannerMessage,
    block_range_scanner::{range_iterator::RangeIterator, reorg_handler::ReorgHandler},
    types::{ChannelState, IntoScannerResult, Notification, ScannerResult, TryStream},
};
use alloy::{
    consensus::BlockHeader,
    eips::BlockNumberOrTag,
    network::{BlockResponse, Network, primitives::HeaderResponse},
    primitives::BlockNumber,
};

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

#[allow(clippy::too_many_arguments)]
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "trace", skip(subscription, sender, provider, reorg_handler))
)]
pub(crate) async fn stream_live_blocks<N: Network, R: ReorgHandler<N>>(
    stream_start: BlockNumber,
    subscription: RobustSubscription<N>,
    sender: &mpsc::Sender<BlockScannerResult>,
    provider: &RobustProvider<N>,
    block_confirmations: u64,
    max_block_range: u64,
    reorg_handler: &mut R,
    notify_after_first_block: bool,
) {
    // Phase 1: Wait for first relevant block
    let mut stream =
        skip_to_first_relevant_block::<N>(subscription, stream_start, block_confirmations);

    let Some(first_block) = get_first_block::<N, _>(&mut stream, sender).await else {
        // error occurred and streamed
        return;
    };

    debug!(
        first_block = first_block.number(),
        stream_start = stream_start,
        "Received first relevant block, starting live streaming"
    );

    // This check is necessary when running `sync` modes. It makes sense to stream this notification
    // only once the first relevant block is received from the subscription, and not before that;
    // otherwise callers might perform certain operations expecting the relevant blocks to start
    // coming, when in fact they are not.
    if notify_after_first_block &&
        sender.try_stream(Notification::SwitchingToLive).await.is_closed()
    {
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

/// Initializes the streaming state after receiving the first block.
/// Returns None if the channel is closed.
async fn initialize_live_streaming_state<N: Network, R: ReorgHandler<N>>(
    first_block: N::HeaderResponse,
    stream_start: BlockNumber,
    block_confirmations: u64,
    max_block_range: u64,
    sender: &mpsc::Sender<BlockScannerResult>,
    provider: &RobustProvider<N>,
    reorg_handler: &mut R,
) -> Option<LiveStreamingState<N>> {
    let confirmed = first_block.number().saturating_sub(block_confirmations);

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
    R: ReorgHandler<N>,
>(
    stream: &mut S,
    state: &mut LiveStreamingState<N>,
    stream_start: BlockNumber,
    block_confirmations: u64,
    max_block_range: u64,
    sender: &mpsc::Sender<BlockScannerResult>,
    provider: &RobustProvider<N>,
    reorg_handler: &mut R,
) {
    while let Some(incoming_block) = stream.next().await {
        let incoming_block = match incoming_block {
            Ok(block) => block,
            Err(e) => {
                match e {
                    subscription::Error::Lagged(_) => {
                        // scanner already accounts for skipped block numbers,
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

        let incoming_block = incoming_block.number();
        trace!(received = incoming_block, "Received item from block subscription");

        let Some(previous_batch_end) = state.previous_batch_end.as_ref() else {
            // previously detected reorg wasn't fully handled
            continue;
        };

        let common_ancestor = match reorg_handler.check(previous_batch_end).await {
            Ok(reorg_opt) => reorg_opt,
            Err(e) => {
                error!("Failed to perform reorg check");
                _ = sender.try_stream(e).await;
                return;
            }
        };

        if let Some(common_ancestor) = common_ancestor {
            if handle_reorg_detected(common_ancestor, stream_start, state, sender).await.is_closed()
            {
                return; // Channel closed
            }
        } else {
            // No reorg: advance batch_start to after the previous batch
            state.batch_start = previous_batch_end.header().number() + 1;
        }

        // Stream the next batch of confirmed blocks
        let batch_end_num = incoming_block.saturating_sub(block_confirmations);
        if stream_next_batch(
            batch_end_num,
            state,
            stream_start,
            max_block_range,
            sender,
            provider,
            reorg_handler,
        )
        .await
        .is_closed()
        {
            return; // Channel closed
        }
    }
}

/// Handles a detected reorg by notifying and adjusting the streaming state.
/// Returns `ChannelState::Closed` if the channel is closed, `ChannelState::Open` otherwise.
async fn handle_reorg_detected<N: Network>(
    common_ancestor: N::BlockResponse,
    stream_start: BlockNumber,
    state: &mut LiveStreamingState<N>,
    sender: &mpsc::Sender<BlockScannerResult>,
) -> ChannelState {
    let ancestor_num = common_ancestor.header().number();

    info!(
        common_ancestor = ancestor_num,
        stream_start = stream_start,
        "Reorg detected during live streaming"
    );

    let channel_state =
        sender.try_stream(Notification::ReorgDetected { common_ancestor: ancestor_num }).await;

    if channel_state.is_closed() {
        return ChannelState::Closed;
    }

    // Reset streaming position based on common ancestor
    if ancestor_num < stream_start {
        // Reorg went before our starting point - restart from stream_start
        debug!(
            common_ancestor = ancestor_num,
            stream_start = stream_start,
            "Reorg predates stream start, restarting from stream_start"
        );
        state.batch_start = stream_start;
        state.previous_batch_end = None;
    } else {
        // Resume from after the common ancestor
        debug!(
            common_ancestor = ancestor_num,
            resume_from = ancestor_num + 1,
            "Resuming from after common ancestor"
        );
        state.batch_start = ancestor_num + 1;
        state.previous_batch_end = Some(common_ancestor);
    }

    ChannelState::Open
}

/// Streams the next batch of blocks up to `batch_end_num`.
/// Returns `ChannelState::Closed` if the channel is closed, `ChannelState::Open` otherwise.
async fn stream_next_batch<N: Network, R: ReorgHandler<N>>(
    batch_end_num: BlockNumber,
    state: &mut LiveStreamingState<N>,
    stream_start: BlockNumber,
    max_block_range: u64,
    sender: &mpsc::Sender<BlockScannerResult>,
    provider: &RobustProvider<N>,
    reorg_handler: &mut R,
) -> ChannelState {
    if batch_end_num < state.batch_start {
        // No new confirmed blocks to stream yet
        return ChannelState::Open;
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
        return ChannelState::Closed;
    }

    // SAFETY: Overflow cannot realistically happen
    state.batch_start = batch_end_num + 1;

    ChannelState::Open
}

/// Tracks the current state of live streaming
struct LiveStreamingState<N: Network> {
    /// The starting block number for the next batch to stream
    batch_start: BlockNumber,
    /// The last block from the previous batch (used for reorg detection)
    previous_batch_end: Option<N::BlockResponse>,
}

#[must_use]
#[allow(clippy::too_many_lines)]
#[cfg_attr(feature = "tracing", tracing::instrument(level = "trace", skip(sender, provider)))]
pub(crate) async fn stream_historical_range<N: Network>(
    start: BlockNumber,
    end: BlockNumber,
    max_block_range: u64,
    sender: &mpsc::Sender<BlockScannerResult>,
    provider: &RobustProvider<N>,
) -> Option<()> {
    // NOTE: Edge case - If the chain is too young to expose finalized blocks (height < finalized
    // depth) just use zero.
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
        trace!(range_start = *range.start(), range_end = *range.end(), "Streaming finalized range");
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
        if sender.try_stream(Notification::ReorgDetected { common_ancestor }).await.is_closed() {
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

/// Assumes that `min_common_ancestor <= next_start_block <= end`, performs no internal checks.
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "trace", skip(sender, provider, reorg_handler))
)]
pub(crate) async fn stream_range_with_reorg_handling<N: Network, R: ReorgHandler<N>>(
    min_common_ancestor: BlockNumber,
    next_start_block: BlockNumber,
    end: BlockNumber,
    max_block_range: u64,
    sender: &mpsc::Sender<BlockScannerResult>,
    provider: &RobustProvider<N>,
    reorg_handler: &mut R,
) -> Option<N::BlockResponse> {
    let mut last_batch_end: Option<N::BlockResponse> = None;
    let mut iter = RangeIterator::forward(next_start_block, end, max_block_range);

    while let Some(batch) = iter.next() {
        let batch_end_num = *batch.end();
        let batch_end = match provider.get_block_by_number(batch_end_num.into()).await {
            Ok(block) => block,
            Err(e) => {
                error!(
                    batch_start = batch.start(),
                    batch_end = batch_end_num,
                    "Failed to get ending block of the current batch"
                );
                _ = sender.try_stream(e).await;
                return None;
            }
        };

        if sender.try_stream(batch).await.is_closed() {
            return None; // channel closed
        }

        let reorged_opt = match reorg_handler.check(&batch_end).await {
            Ok(opt) => opt,
            Err(e) => {
                error!("Failed to perform reorg check");
                _ = sender.try_stream(e).await;
                return None;
            }
        };

        if let Some(common_ancestor) = reorged_opt {
            let common_ancestor = common_ancestor.header().number();
            info!(
                common_ancestor = common_ancestor,
                "Reorg detected during historical streaming, resetting range iterator"
            );
            if sender.try_stream(Notification::ReorgDetected { common_ancestor }).await.is_closed()
            {
                return None;
            }
            let reset_to = (common_ancestor + 1).max(min_common_ancestor);
            debug!(reset_to = reset_to, "Resetting range iterator after reorg");
            iter.reset_to(reset_to);
        }

        last_batch_end = Some(batch_end);
    }

    last_batch_end
}
