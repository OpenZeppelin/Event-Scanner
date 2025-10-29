use std::ops::RangeInclusive;

use crate::{
    block_range_scanner::{MAX_BUFFERED_MESSAGES, Message as BlockRangeMessage},
    event_scanner::{filter::EventFilter, listener::EventListener},
    robust_provider::{Error as RobustProviderError, RobustProvider},
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
/// - `Stream` mode: consumers forward logs immediately as they arrive
/// - `CollectLatest` mode: consumers accumulate logs and send them only at the end
///
/// This tight coupling means consumers cannot be reused across different modes. For example,
/// the "sync from latest" scanning strategy needs to run two modes sequentially (first
/// `CollectLatest` to get recent events, then `Stream` for ongoing events), requiring separate
/// consumer spawns for each phase rather than reusing the same consumers.
///
/// # Note
///
/// Assumes it is running in a separate tokio task, so as to be non-blocking.
pub async fn handle_stream<N: Network, S: Stream<Item = BlockRangeMessage> + Unpin>(
    mut stream: S,
    provider: &RootProvider<N>,
    listeners: &[EventListener],
    mode: ConsumerMode,
) {
    let (range_tx, _) = broadcast::channel::<BlockRangeMessage>(MAX_BUFFERED_MESSAGES);

    let consumers = spawn_log_consumers(provider, listeners, &range_tx, mode);

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
pub fn spawn_log_consumers<N: Network>(
    provider: &RobustProvider<N>,
    listeners: &[EventListener],
    range_tx: &Sender<BlockRangeMessage>,
    mode: ConsumerMode,
) -> JoinSet<()> {
    listeners.iter().cloned().fold(JoinSet::new(), |mut set, listener| {
        let EventListener { filter, sender } = listener;

        let provider = provider.clone();
        let base_filter = Filter::from(&filter);
        let mut range_rx = range_tx.subscribe();

        set.spawn(async move {
            // Only used for CollectLatest
            let mut collected: Vec<Log> = match mode {
                ConsumerMode::CollectLatest { count } => Vec::with_capacity(count),
                ConsumerMode::Stream => Vec::new(),
            };

            loop {
                match range_rx.recv().await {
                    Ok(BlockRangeMessage::Data(range)) => {
                        match get_logs(range, &filter, &base_filter, &provider).await {
                            Ok(logs) => {
                                if logs.is_empty() {
                                    continue;
                                }

                                match mode {
                                    ConsumerMode::Stream => {
                                        if !sender.try_stream(logs).await {
                                            break;
                                        }
                                    }
                                    ConsumerMode::CollectLatest { count } => {
                                        let take = count.saturating_sub(collected.len());
                                        // if we have enough logs, break
                                        if take == 0 {
                                            break;
                                        }
                                        // take latest within this range
                                        collected.extend(logs.into_iter().rev().take(take));
                                        // if we have enough logs, break
                                        if collected.len() == count {
                                            break;
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                if !sender.try_stream(e).await {
                                    break;
                                }
                            }
                        }
                    }
                    Ok(BlockRangeMessage::Error(e)) => {
                        error!(error = ?e, "Received error message");
                        if !sender.try_stream(e).await {
                            break;
                        }
                    }
                    Ok(BlockRangeMessage::Status(status)) => {
                        info!(status = ?status, "Received status message");
                        if !sender.try_stream(status).await {
                            break;
                        }
                    }
                    Err(RecvError::Closed) => {
                        info!("No block ranges to receive, dropping receiver.");
                        break;
                    }
                    Err(RecvError::Lagged(_)) => {}
                }
            }

            if let ConsumerMode::CollectLatest { .. } = mode {
                if !collected.is_empty() {
                    collected.reverse(); // restore chronological order
                }

                info!("Sending collected logs to consumer");
                _ = sender.try_stream(collected).await;
            }
        });

        set
    })
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
