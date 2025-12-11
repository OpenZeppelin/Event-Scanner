use std::ops::RangeInclusive;

use crate::{
    Notification, ScannerMessage,
    block_range_scanner::{BlockScannerResult, MAX_BUFFERED_MESSAGES},
    event_scanner::{filter::EventFilter, listener::EventListener},
    robust_provider::{RobustProvider, provider::Error as RobustProviderError},
    types::TryStream,
};
use alloy::{
    network::Network,
    rpc::types::{Filter, Log},
};
use tokio::{
    sync::broadcast::{self, Sender, error::RecvError},
    task::JoinSet,
};
use tokio_stream::{Stream, StreamExt};
use tracing::{error, info, warn};

#[derive(Copy, Clone, Debug)]
pub enum ConsumerMode {
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
pub async fn handle_stream<N: Network, S: Stream<Item = BlockScannerResult> + Unpin>(
    mut stream: S,
    provider: &RobustProvider<N>,
    listeners: &[EventListener],
    mode: ConsumerMode,
) {
    let (range_tx, _) = broadcast::channel::<BlockScannerResult>(MAX_BUFFERED_MESSAGES);

    let consumers = match mode {
        ConsumerMode::Stream => spawn_log_consumers_in_stream_mode(provider, listeners, &range_tx),
        ConsumerMode::CollectLatest { count } => {
            spawn_log_consumers_in_collection_mode(provider, listeners, &range_tx, count)
        }
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
pub fn spawn_log_consumers_in_stream_mode<N: Network>(
    provider: &RobustProvider<N>,
    listeners: &[EventListener],
    range_tx: &Sender<BlockScannerResult>,
) -> JoinSet<()> {
    listeners.iter().cloned().fold(JoinSet::new(), |mut set, listener| {
        let EventListener { filter, sender } = listener;

        let provider = provider.clone();
        let base_filter = Filter::from(&filter);
        let mut range_rx = range_tx.subscribe();

        set.spawn(async move {
            loop {
                match range_rx.recv().await {
                    Ok(message) => match message {
                        Ok(ScannerMessage::Data(range)) => {
                            match get_logs(range, &filter, &base_filter, &provider).await {
                                Ok(logs) => {
                                    if logs.is_empty() {
                                        continue;
                                    }

                                    if !sender.try_stream(logs).await {
                                        return;
                                    }
                                }
                                Err(e) => {
                                    error!(error = ?e, "Received error message");
                                    if !sender.try_stream(e).await {
                                        return;
                                    }
                                }
                            }
                        }
                        Ok(ScannerMessage::Notification(notification)) => {
                            info!(notification = ?notification, "Received notification");
                            if !sender.try_stream(notification).await {
                                return;
                            }
                        }
                        Err(e) => {
                            error!(error = ?e, "Received error message");
                            if !sender.try_stream(e).await {
                                return;
                            }
                        }
                    },
                    Err(RecvError::Closed) => {
                        info!("No block ranges to receive, dropping receiver.");
                        break;
                    }
                    Err(RecvError::Lagged(_)) => {}
                }
            }
        });

        set
    })
}

#[must_use]
pub fn spawn_log_consumers_in_collection_mode<N: Network>(
    provider: &RobustProvider<N>,
    listeners: &[EventListener],
    range_tx: &Sender<BlockScannerResult>,
    count: usize,
) -> JoinSet<()> {
    listeners.iter().cloned().fold(JoinSet::new(), |mut set, listener| {
        let EventListener { filter, sender } = listener;

        let provider = provider.clone();
        let base_filter = Filter::from(&filter);
        let mut range_rx = range_tx.subscribe();

        set.spawn(async move {
            // Only used for CollectLatest
            let mut collected = Vec::with_capacity(count);

            // Tracks common ancestor block during reorg recovery for proper log ordering
            let mut reorg_ancestor: Option<u64> = None;

            loop {
                match range_rx.recv().await {
                    Ok(message) => match message {
                        Ok(ScannerMessage::Data(range)) => {
                            let range_end = *range.end();
                            match get_logs(range, &filter, &base_filter, &provider).await {
                                Ok(logs) => {
                                    if logs.is_empty() {
                                        continue;
                                    }

                                    // Check if in reorg recovery and past the reorg range
                                    if reorg_ancestor.is_some_and(|a| range_end <= a) {
                                        info!(
                                            ancestor = reorg_ancestor,
                                            range_end = range_end,
                                            "Reorg recovery complete, resuming normal log collection"
                                        );
                                        reorg_ancestor = None;
                                    }

                                    let should_prepend = reorg_ancestor.is_some();
                                    if collect_logs(&mut collected, logs, count, should_prepend) {
                                        break;
                                    }
                                }
                                Err(e) => {
                                    error!(error = ?e, "Received error message");
                                    if !sender.try_stream(e).await {
                                        return; // channel closed
                                    }
                                }
                            }
                        }
                        Ok(ScannerMessage::Notification(Notification::ReorgDetected {
                            common_ancestor,
                        })) => {
                            info!(
                                common_ancestor = common_ancestor,
                                "Received ReorgDetected notification"
                            );

                            // Invalidate logs from reorged blocks
                            // Logs are ordered newest -> oldest, so skip logs with
                            // block_number > common_ancestor at the front
                            // NOTE: Pending logs are not supported therefore this filter
                            // works for now (may need to update once they are). Tracked in
                            // <https://github.com/OpenZeppelin/Event-Scanner/issues/244>
                            let before_count = collected.len();
                            collected = collected
                                .into_iter()
                                .skip_while(|log| {
                                    // Pending blocks aren't supported therefore this filter
                                    // works for now (may need to update once they are).
                                    // Tracked in <https://github.com/OpenZeppelin/Event-Scanner/issues/244>
                                    log.block_number.is_some_and(|n| n > common_ancestor)
                                })
                                .collect();
                            let removed_count = before_count - collected.len();
                            if removed_count > 0 {
                                info!(
                                    removed_count = removed_count,
                                    remaining_count = collected.len(),
                                    "Invalidated logs from reorged blocks"
                                );
                            }

                            // Track reorg state for proper log ordering
                            reorg_ancestor = Some(common_ancestor);

                            // Don't forward the notification to the user in CollectLatest mode
                            // since logs haven't been sent yet
                        }
                        Ok(ScannerMessage::Notification(notification)) => {
                            info!(notification = ?notification, "Received notification");
                            if !sender.try_stream(notification).await {
                                return;
                            }
                        }
                        Err(e) => {
                            error!(error = ?e, "Received error message");
                            if !sender.try_stream(e).await {
                                return;
                            }
                        }
                    },
                    Err(RecvError::Closed) => {
                        info!("No block ranges to receive, dropping receiver.");
                        break;
                    }
                    Err(RecvError::Lagged(_)) => {}
                }
            }

            if collected.is_empty() {
                info!("No logs found");
                _ = sender.try_stream(Notification::NoPastLogsFound).await;
                return;
            }

            info!(count = collected.len(), "Logs found");
            collected.reverse(); // restore chronological order

            info!("Sending collected logs to consumer");
            _ = sender.try_stream(collected).await;
        });

        set
    })
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
