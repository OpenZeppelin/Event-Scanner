use std::ops::RangeInclusive;

use crate::{
    block_range_scanner::{MAX_BUFFERED_MESSAGES, Message as BlockRangeMessage},
    event_scanner::{filter::EventFilter, listener::EventListener},
    types::TryStream,
};
use alloy::{
    network::Network,
    providers::{Provider, RootProvider},
    rpc::types::{Filter, Log},
    transports::{RpcError, TransportErrorKind},
};
use tokio::sync::broadcast::{self, Sender, error::RecvError};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tracing::{error, info};

#[derive(Copy, Clone)]
pub enum ConsumerMode {
    Stream,
    CollectLatest { count: usize },
}

pub async fn handle_stream<N: Network>(
    mut stream: ReceiverStream<BlockRangeMessage>,
    provider: &RootProvider<N>,
    listeners: &[EventListener],
    mode: ConsumerMode,
) {
    let (range_tx, _) = broadcast::channel::<BlockRangeMessage>(MAX_BUFFERED_MESSAGES);

    spawn_log_consumers(provider, listeners, &range_tx, mode);

    tokio::spawn(async move {
        while let Some(message) = stream.next().await {
            if let Err(err) = range_tx.send(message) {
                error!(error = %err, "No receivers, stopping broadcast");
                break;
            }
        }
    });
}

pub fn spawn_log_consumers<N: Network>(
    provider: &RootProvider<N>,
    listeners: &[EventListener],
    range_tx: &Sender<BlockRangeMessage>,
    mode: ConsumerMode,
) {
    for listener in listeners.iter().cloned() {
        let EventListener { filter, sender } = listener;

        let provider = provider.clone();
        let base_filter = Filter::from(&filter);
        let mut range_rx = range_tx.subscribe();

        tokio::spawn(async move {
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
                                        if take == 0 {
                                            break;
                                        }
                                        // take latest within this range
                                        collected.extend(logs.into_iter().rev().take(take));
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
                        if !sender.try_stream(e).await {
                            break;
                        }
                    }
                    Ok(BlockRangeMessage::Status(status)) => {
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

                _ = sender.try_stream(collected).await;
            }
        });
    }
}

async fn get_logs<N: Network>(
    range: RangeInclusive<u64>,
    event_filter: &EventFilter,
    log_filter: &Filter,
    provider: &RootProvider<N>,
) -> Result<Vec<Log>, RpcError<TransportErrorKind>> {
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
