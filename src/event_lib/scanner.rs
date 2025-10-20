use std::ops::RangeInclusive;

use crate::{
    EventScannerError,
    block_range_scanner::{
        BlockRangeMessage, BlockRangeScannerError, ConnectedBlockRangeScanner,
        MAX_BUFFERED_MESSAGES,
    },
    event_lib::{filter::EventFilter, listener::EventListener},
    types::ScannerMessage,
};
use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    providers::{Provider, RootProvider},
    rpc::types::{Filter, Log},
    sol_types::SolEvent,
    transports::{RpcError, TransportErrorKind},
};
use tokio::sync::{
    broadcast::{self, Sender, error::RecvError},
    mpsc,
};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tracing::{error, info, warn};

pub type EventScannerMessage = ScannerMessage<Vec<Log>, EventScannerError>;

pub struct EventScannerService<N: Network> {
    block_range_scanner: ConnectedBlockRangeScanner<N>,
    event_listeners: Vec<EventListener>,
}

impl From<Result<Vec<Log>, RpcError<TransportErrorKind>>> for EventScannerMessage {
    fn from(logs: Result<Vec<Log>, RpcError<TransportErrorKind>>) -> Self {
        match logs {
            Ok(logs) => EventScannerMessage::Data(logs),
            Err(e) => EventScannerMessage::Error(e.into()),
        }
    }
}

impl From<Vec<Log>> for EventScannerMessage {
    fn from(logs: Vec<Log>) -> Self {
        EventScannerMessage::Data(logs)
    }
}

impl<E: SolEvent> PartialEq<Vec<E>> for EventScannerMessage {
    fn eq(&self, other: &Vec<E>) -> bool {
        self.eq(&other.as_slice())
    }
}

impl<E: SolEvent> PartialEq<&Vec<E>> for EventScannerMessage {
    fn eq(&self, other: &&Vec<E>) -> bool {
        self.eq(&other.as_slice())
    }
}

impl<E: SolEvent, const N: usize> PartialEq<&[E; N]> for EventScannerMessage {
    fn eq(&self, other: &&[E; N]) -> bool {
        self.eq(&other.as_slice())
    }
}

impl<E: SolEvent> PartialEq<&[E]> for EventScannerMessage {
    fn eq(&self, other: &&[E]) -> bool {
        if let EventScannerMessage::Data(logs) = self {
            logs.iter().map(|l| l.data().clone()).eq(other.iter().map(SolEvent::encode_log_data))
        } else {
            false
        }
    }
}

#[derive(Copy, Clone)]
enum ConsumerMode {
    Stream,
    CollectLatest { count: usize },
}

impl<N: Network> EventScannerService<N> {
    pub(crate) fn from_config(block_range_scanner: ConnectedBlockRangeScanner<N>) -> Self {
        Self { block_range_scanner, event_listeners: Vec::new() }
    }

    pub fn create_event_stream(
        &mut self,
        event_filter: EventFilter,
    ) -> ReceiverStream<EventScannerMessage> {
        let (sender, receiver) = mpsc::channel::<EventScannerMessage>(MAX_BUFFERED_MESSAGES);
        self.add_event_listener(EventListener { filter: event_filter, sender });
        ReceiverStream::new(receiver)
    }

    /// Implementation of stream live
    ///
    /// # Reorg behavior
    ///
    /// * Emits [`ScannerStatus::ReorgDetected`] and adjusts the next block range using
    ///   `block_confirmations` to re-emit the confirmed portion.
    ///
    /// # Errors
    ///
    /// * `EventScannerMessage::ServiceShutdown` - if the service is already shutting down.
    pub async fn stream_live(&self, block_confirmations: u64) -> Result<(), EventScannerError> {
        let client = self.block_range_scanner.run()?;
        let stream = client.stream_live(block_confirmations).await?;

        self.handle_stream(stream, ConsumerMode::Stream).await;

        Ok(())
    }

    /// Implementation of stream historical
    ///
    /// # Errors
    ///
    /// * `EventScannerMessage::ServiceShutdown` - if the service is already shutting down.
    pub async fn stream_historical<T: Into<BlockNumberOrTag>>(
        &self,
        start_height: T,
        end_height: T,
    ) -> Result<(), EventScannerError> {
        let client = self.block_range_scanner.run()?;
        let stream = client.stream_historical(start_height, end_height).await?;

        self.handle_stream(stream, ConsumerMode::Stream).await;

        Ok(())
    }

    /// Implementation of stream from
    ///
    /// # Errors
    ///
    /// * `EventScannerMessage::ServiceShutdown` - if the service is already shutting down.
    pub async fn stream_from(
        &self,
        start_height: BlockNumberOrTag,
        block_confirmations: u64,
    ) -> Result<(), EventScannerError> {
        let client = self.block_range_scanner.run()?;
        let stream = client.stream_from(start_height, block_confirmations).await?;

        self.handle_stream(stream, ConsumerMode::Stream).await;

        Ok(())
    }

    /// Scans a block range and collects the latest `count` matching events per registered listener.
    ///
    /// Emits a single message per listener with up to `count` logs, ordered oldest→newest.
    ///
    /// # Arguments
    ///
    /// * `count` - Maximum number of events to return per listener.
    /// * `start_height` - Inclusive start block (tag or number).
    /// * `end_height` - Inclusive end block (tag or number).
    ///
    /// # Reorg behavior
    ///
    /// Performs a reverse-ordered rewind over the range, periodically checking the tip hash. If a
    /// reorg is detected, emits [`ScannerStatus::ReorgDetected`], resets the rewind start to the
    /// updated tip, and resumes until completion. Final log delivery preserves chronological order.
    ///
    /// # Errors
    ///
    /// * Returns `EventScannerError` if the scanner fails to start or fetching logs fails.
    ///
    /// [`ScannerStatus::ReorgDetected`]: crate::types::ScannerStatus::ReorgDetected
    pub async fn stream_latest<T: Into<BlockNumberOrTag>>(
        self,
        count: usize,
        start_height: T,
        end_height: T,
    ) -> Result<(), EventScannerError> {
        let client = self.block_range_scanner.run()?;
        let stream = client.rewind(start_height, end_height).await?;

        self.handle_stream(stream, ConsumerMode::CollectLatest { count }).await;

        Ok(())
    }

    async fn handle_stream(
        &self,
        mut stream: ReceiverStream<ScannerMessage<RangeInclusive<u64>, BlockRangeScannerError>>,
        mode: ConsumerMode,
    ) {
        let (range_tx, _) = broadcast::channel::<BlockRangeMessage>(MAX_BUFFERED_MESSAGES);

        self.spawn_log_consumers(&range_tx, mode);

        while let Some(message) = stream.next().await {
            if let Err(err) = range_tx.send(message) {
                error!(error = %err, "No receivers, stopping broadcast");
                break;
            }
        }
    }

    fn spawn_log_consumers(&self, range_tx: &Sender<BlockRangeMessage>, mode: ConsumerMode) {
        for listener in &self.event_listeners {
            let provider = self.block_range_scanner.provider().clone();
            let filter = listener.filter.clone();
            let base_filter = Filter::from(&filter);
            let sender = listener.sender.clone();
            let mut sub = range_tx.subscribe();

            tokio::spawn(async move {
                // Only used for CollectLatest
                let mut collected: Vec<Log> = match mode {
                    ConsumerMode::CollectLatest { count } => Vec::with_capacity(count),
                    ConsumerMode::Stream => Vec::new(),
                };

                loop {
                    match sub.recv().await {
                        Ok(BlockRangeMessage::Data(range)) => {
                            match Self::get_logs(range, &filter, &base_filter, &provider).await {
                                Ok(logs) => {
                                    if logs.is_empty() {
                                        continue;
                                    }

                                    match mode {
                                        ConsumerMode::Stream => {
                                            if !Self::try_send(&sender, logs).await {
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
                                    if !Self::try_send(&sender, e).await {
                                        break;
                                    }
                                }
                            }
                        }
                        Ok(BlockRangeMessage::Error(e)) => {
                            if !Self::try_send(&sender, e).await {
                                break;
                            }
                        }
                        Ok(BlockRangeMessage::Status(status)) => {
                            if !Self::try_send(&sender, status).await {
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

                    _ = Self::try_send(&sender, collected).await;
                }
            });
        }
    }

    async fn get_logs(
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

    async fn try_send<T: Into<EventScannerMessage>>(
        sender: &mpsc::Sender<EventScannerMessage>,
        msg: T,
    ) -> bool {
        if let Err(err) = sender.send(msg.into()).await {
            warn!(error = %err, "Downstream channel closed, stopping stream");
            return false;
        }
        true
    }

    fn add_event_listener(&mut self, event_listener: EventListener) {
        self.event_listeners.push(event_listener);
    }
}
