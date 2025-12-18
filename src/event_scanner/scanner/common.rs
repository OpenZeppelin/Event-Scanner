use std::ops::RangeInclusive;

use crate::{
    Message, Notification, ScannerError, ScannerMessage,
    block_range_scanner::BlockScannerResult,
    event_scanner::{filter::EventFilter, listener::EventListener},
    robust_provider::{RobustProvider, provider::Error as RobustProviderError},
    types::TryStream,
};
use alloy::{
    network::Network,
    rpc::types::{Filter, Log},
};
use futures::StreamExt;
use tokio::{
    sync::{
        broadcast::{self, Sender, error::RecvError},
        mpsc,
    },
    task::JoinSet,
};
use tokio_stream::{Stream, wrappers::ReceiverStream};

#[derive(Copy, Clone, Debug)]
pub(crate) enum ConsumerMode {
    Stream,
    CollectLatest { count: usize },
}

/// Orchestrates the consumption of block range messages from a stream and dispatches them to
/// event log consumers.
///
/// This function sets up a broadcast channel to distribute block range messages from the input
/// stream to multiple log consumers (one per event listener). Each consumer fetches logs for
/// their specific event filter and handles them according to the specified mode.
///
/// # Why this design?
///
/// Log consumers are tightly coupled with the `ConsumerMode` because the mode dictates their
/// entire lifecycle and behavior:
/// * `Stream` mode: consumers forward logs immediately as they arrive
/// * `CollectLatest` mode: consumers accumulate logs and send them only at the end
///
/// This tight coupling means consumers cannot be reused across different modes. For example,
/// the "sync from latest" scanning strategy needs to run two modes sequentially (first
/// `CollectLatest` to get recent events, then `Stream` for ongoing events), requiring separate
/// consumer spawns for each phase rather than reusing the same consumers.
///
/// # Note
///
/// Assumes it is running in a separate tokio task, so as to be non-blocking.
pub(crate) async fn handle_stream<N: Network, S: Stream<Item = BlockScannerResult> + Unpin>(
    mut stream: S,
    provider: &RobustProvider<N>,
    listeners: &[EventListener],
    mode: ConsumerMode,
    max_concurrent_fetches: usize,
    buffer_capacity: usize,
) {
    let (range_tx, _) = broadcast::channel::<BlockScannerResult>(buffer_capacity);

    let consumers = match mode {
        ConsumerMode::Stream => spawn_log_consumers_in_stream_mode(
            provider,
            listeners,
            &range_tx,
            max_concurrent_fetches,
        ),
        ConsumerMode::CollectLatest { count } => spawn_log_consumers_in_collection_mode(
            provider,
            listeners,
            &range_tx,
            count,
            max_concurrent_fetches,
        ),
    };

    while let Some(message) = stream.next().await {
        if let Err(err) = range_tx.send(message) {
            warn!(error = %err, "No log consumers, stopping stream");
            break;
        }
    }

    // Close the channel sender to signal to the log consumers that streaming is done.
    drop(range_tx);

    // ensure all consumers finish before they're dropped
    consumers.join_all().await;
}

#[must_use]
fn spawn_log_consumers_in_stream_mode<N: Network>(
    provider: &RobustProvider<N>,
    listeners: &[EventListener],
    range_tx: &Sender<BlockScannerResult>,
    max_concurrent_fetches: usize,
) -> JoinSet<()> {
    listeners.iter().cloned().fold(JoinSet::new(), |mut set, listener| {
        let EventListener { filter, sender } = listener;

        let provider = provider.clone();
        let base_filter = Filter::from(&filter);
        let mut range_rx = range_tx.subscribe();

        set.spawn(async move {
            // We use a channel and convert the receiver to a stream because it already has a
            // convenience function `buffered` for concurrently handling block ranges, while
            // outputting results in the same order as they were received.
            let (tx, rx) = mpsc::channel::<BlockScannerResult>(max_concurrent_fetches);

            // Process block ranges concurrently in a separate thread so that the current thread can
            // continue receiving and buffering subsequent block ranges while the previous ones are
            // being processed.
            tokio::spawn(async move {
                let mut stream = ReceiverStream::new(rx)
                    .map(async |message| match message {
                        Ok(ScannerMessage::Data(range)) => {
                            get_logs(range, &filter, &base_filter, &provider)
                                .await
                                .map(Message::from)
                                .map_err(ScannerError::from)
                        }
                        Ok(ScannerMessage::Notification(notification)) => Ok(notification.into()),
                        // No need to stop the stream on an error, because that decision is up to
                        // the caller.
                        Err(e) => Err(e),
                    })
                    .buffered(max_concurrent_fetches);

                // process all of the buffered results
                while let Some(result) = stream.next().await {
                    if let Ok(ScannerMessage::Data(logs)) = result.as_ref() &&
                        logs.is_empty()
                    {
                        continue;
                    }

                    if !sender.try_stream(result).await {
                        return;
                    }
                }
            });

            // Receive block ranges from the broadcast channel and send them to the range processor
            // for parallel processing.
            loop {
                match range_rx.recv().await {
                    Ok(message) => {
                        tx.send(message).await.expect("receiver dropped only if we exit this loop");
                    }
                    Err(RecvError::Closed) => {
                        debug!("No more block ranges to receive");
                        break;
                    }
                    Err(RecvError::Lagged(skipped)) => {
                        debug!(skipped_messages = skipped, "Channel lagged");
                    }
                }
            }

            // Drop the local channel sender to signal to the range processor that streaming is
            // done.
            drop(tx);
        });

        set
    })
}

#[must_use]
fn spawn_log_consumers_in_collection_mode<N: Network>(
    provider: &RobustProvider<N>,
    listeners: &[EventListener],
    range_tx: &Sender<BlockScannerResult>,
    count: usize,
    max_concurrent_fetches: usize,
) -> JoinSet<()> {
    listeners.iter().cloned().fold(JoinSet::new(), |mut set, listener| {
        let EventListener { filter, sender } = listener;

        let provider = provider.clone();
        let base_filter = Filter::from(&filter);
        let mut range_rx = range_tx.subscribe();

        set.spawn(async move {
            // We use a channel and convert the receiver to a stream because it already has a
            // convenience function `buffered` for concurrently handling block ranges, while
            // outputting results in the same order as they were received.
            let (tx, rx) = mpsc::channel::<BlockScannerResult>(max_concurrent_fetches);

            // Process block ranges concurrently in a separate thread so that the current thread can
            // continue receiving and buffering subsequent block ranges while the previous ones are
            // being processed.
            tokio::spawn(async move {
                let mut stream = ReceiverStream::new(rx)
                    .map(async |message| match message {
                        Ok(ScannerMessage::Data(range)) => {
                            get_logs(range, &filter, &base_filter, &provider)
                                .await
                                .map(Message::from)
                                .map_err(ScannerError::from)
                        }
                        Ok(ScannerMessage::Notification(notification)) => Ok(notification.into()),
                        // No need to stop the stream on an error, because that decision is up to
                        // the caller.
                        Err(e) => Err(e),
                    })
                    .buffered(max_concurrent_fetches);

                let mut collected = Vec::with_capacity(count);

                // Tracks common ancestor block during reorg recovery for proper log ordering
                let mut reorg_ancestor: Option<u64> = None;

                // process all of the buffered results
                while let Some(result) = stream.next().await {
                    match result {
                        Ok(ScannerMessage::Data(logs)) => {
                            if logs.is_empty() {
                                continue;
                            }

                            let last_log_block_num = logs
                                .last()
                                .expect("logs is not empty")
                                .block_number
                                .expect("pending blocks not supported");
                            // Check if in reorg recovery and past the reorg range
                            if reorg_ancestor.is_some_and(|a| last_log_block_num <= a) {
                                debug!(
                                    ancestor = reorg_ancestor,
                                    "Reorg recovery complete, resuming normal log collection"
                                );
                                reorg_ancestor = None;
                            }

                            let should_prepend = reorg_ancestor.is_some();
                            if collect_logs(&mut collected, logs, count, should_prepend) {
                                break;
                            }
                        }

                        Ok(ScannerMessage::Notification(Notification::ReorgDetected {
                            common_ancestor,
                        })) => {
                            debug!(
                                common_ancestor = common_ancestor,
                                "Received ReorgDetected notification"
                            );
                            // Track reorg state for proper log ordering
                            reorg_ancestor = Some(common_ancestor);

                            collected =
                                discard_logs_from_orphaned_blocks(collected, common_ancestor);

                            // Don't forward the notification to the user in CollectLatest mode
                            // since logs haven't been sent yet
                        }
                        Ok(ScannerMessage::Notification(notification)) => {
                            debug!(notification = ?notification, "Received notification");
                            if !sender.try_stream(notification).await {
                                return;
                            }
                        }
                        Err(e) => {
                            if !sender.try_stream(e).await {
                                return;
                            }
                        }
                    }
                }

                if collected.is_empty() {
                    debug!("No logs found");
                    _ = sender.try_stream(Notification::NoPastLogsFound).await;
                    return;
                }

                trace!(count = collected.len(), "Logs found");
                collected.reverse(); // restore chronological order

                trace!("Sending collected logs to consumer");
                _ = sender.try_stream(collected).await;
            });

            // Receive block ranges from the broadcast channel and send them to the range processor
            // for parallel processing.
            loop {
                match range_rx.recv().await {
                    Ok(message) => {
                        if tx.send(message).await.is_err() {
                            // range processor has streamed the expected number of logs, stop
                            // sending ranges
                            break;
                        }
                    }
                    Err(RecvError::Closed) => {
                        debug!("No more block ranges to receive");
                        break;
                    }
                    Err(RecvError::Lagged(skipped)) => {
                        debug!(skipped_messages = skipped, "Channel lagged");
                    }
                }
            }

            // Drop the local channel sender to signal to the range processor that streaming is
            // done.
            drop(tx);
        });

        set
    })
}

fn discard_logs_from_orphaned_blocks(collected: Vec<Log>, common_ancestor: u64) -> Vec<Log> {
    // Invalidate logs from reorged blocks
    // Logs are ordered newest -> oldest, so skip logs with
    // block_number > common_ancestor at the front
    // NOTE: Pending logs are not supported therefore this filter
    // works for now (may need to update once they are). Tracked in
    // <https://github.com/OpenZeppelin/Event-Scanner/issues/244>
    let before_count = collected.len();
    let collected = collected
        .into_iter()
        .skip_while(|log| {
            // Pending blocks aren't supported therefore this filter
            // works for now (may need to update once they are).
            // Tracked in <https://github.com/OpenZeppelin/Event-Scanner/issues/244>
            log.block_number.is_some_and(|n| n > common_ancestor)
        })
        .collect::<Vec<_>>();
    let removed_count = before_count - collected.len();
    if removed_count > 0 {
        debug!(
            removed_count = removed_count,
            remaining_count = collected.len(),
            "Invalidated logs from reorged blocks"
        );
    }
    collected
}

/// Collects logs into the buffer, either prepending (reorg recovery) or appending (normal).
/// Returns `true` if collection is complete (reached count limit).
fn collect_logs<T>(collected: &mut Vec<T>, logs: Vec<T>, count: usize, prepend: bool) -> bool {
    if prepend {
        // Reorg rescan ranges are sent in ascending order (oldest → latest), opposite to normal
        // rewind which sends descending (latest → oldest). This means each successive reorg batch
        // contains newer blocks, so we always prepend at position 0 to maintain newest-first order.
        // Example: reorg rescan sends 86..=95 then 96..=100
        //   - First batch (86..=95): prepend → [95, 94, ..., 86]
        //   - Second batch (96..=100): prepend → [100, 99, ..., 96, 95, 94, ..., 86]
        let new_logs = logs.into_iter().rev().take(count);
        let keep = count.saturating_sub(new_logs.len());
        collected.truncate(keep);
        collected.splice(..0, new_logs);
    } else {
        // Normal: append up to remaining capacity
        let take = count.saturating_sub(collected.len());
        if take == 0 {
            return true;
        }
        collected.extend(logs.into_iter().rev().take(take));
    }

    collected.len() >= count
}

async fn get_logs<N: Network>(
    range: RangeInclusive<u64>,
    event_filter: &EventFilter,
    log_filter: &Filter,
    provider: &RobustProvider<N>,
) -> Result<Vec<Log>, RobustProviderError> {
    let log_filter = log_filter.clone().from_block(*range.start()).to_block(*range.end());

    match provider.get_logs(&log_filter).await {
        Ok(logs) => {
            if logs.is_empty() {
                return Ok(logs);
            }

            info!(
                filter = %event_filter,
                log_count = logs.len(),
                block_range = ?range,
                "found logs for event in block range"
            );

            Ok(logs)
        }
        Err(e) => {
            error!(
                filter = %event_filter,
                error = %e,
                block_range = ?range,
                "failed to get logs for block range"
            );

            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_logs_appends_in_reverse_order() {
        let mut collected = vec![];
        let new_logs = vec![10, 11, 12];

        let done = collect_logs(&mut collected, new_logs, 5, false);

        assert!(!done);
        // logs are reversed (newest first): 12, 11, 10
        assert_eq!(collected, vec![12, 11, 10]);
    }

    #[test]
    fn collect_logs_prepends_in_reverse_order() {
        let mut collected = vec![];
        let new_logs = vec![10, 11, 12];

        let done = collect_logs(&mut collected, new_logs, 5, true);

        assert!(!done);
        // logs are reversed (newest first): 12, 11, 10
        assert_eq!(collected, vec![12, 11, 10]);
    }

    #[test]
    fn collect_logs_stops_at_count() {
        let mut collected = vec![15, 14];
        let new_logs = vec![10, 11, 12, 13];

        let done = collect_logs(&mut collected, new_logs, 5, false);

        assert!(done);
        // takes only 3 more (count=5, had 2), reversed: 13, 12, 11
        assert_eq!(collected, vec![15, 14, 13, 12, 11]);
    }

    #[test]
    fn collect_logs_prepends_during_reorg_recovery() {
        // Had logs from blocks 75, 70
        // Reorg at block 80, now getting replacement logs for 85, 90
        let mut collected = vec![75, 70];
        let new_logs = vec![85, 90];

        let done = collect_logs(&mut collected, new_logs, 5, true);

        assert!(!done);
        // prepended (reversed): 90, 85, then existing: 75, 70
        assert_eq!(collected, vec![90, 85, 75, 70]);
    }

    #[test]
    fn collect_logs_prioritizes_prepended_logs_when_truncating() {
        // Had 4 logs, count=5, prepending 3 new logs
        let mut collected = vec![75, 70, 65, 60];
        let new_logs = vec![85, 90, 95];

        let done = collect_logs(&mut collected, new_logs, 5, true);

        assert!(done);
        // All 3 new logs prepended (reversed: 95,90,85)
        // [95, 90, 85, 75, 70] (60 dropped as oldest)
        assert_eq!(collected, vec![95, 90, 85, 75, 70]);

        // edge case: more incoming logs than collected
        let mut collected = vec![75, 70, 65, 60];
        let new_logs = vec![85, 90, 95, 100, 105];

        let done = collect_logs(&mut collected, new_logs, 5, true);

        assert!(done);
        // [105, 100, 95, 90, 85] (all old collected logs dropped)
        assert_eq!(collected, vec![105, 100, 95, 90, 85]);
    }

    #[test]
    fn collect_logs_ignores_new_logs_for_appending_when_already_at_count() {
        let mut collected = vec![100, 99, 98];
        let new_logs = vec![90];

        let done = collect_logs(&mut collected, new_logs, 3, false);

        assert!(done);
        assert_eq!(collected, vec![100, 99, 98]);
    }

    #[test]
    fn collect_logs_prepend_respects_count_limit() {
        // count=3, have 1, prepending 4 logs
        let mut collected = vec![70];
        let new_logs = vec![80, 85, 90, 95];

        let done = collect_logs(&mut collected, new_logs, 3, true);

        assert!(done);

        assert_eq!(collected, vec![95, 90, 85]);
    }
}
