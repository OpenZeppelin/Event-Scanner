use std::{ops::RangeInclusive, sync::Arc};

use crate::{
    block_range_scanner::{
        BlockRangeMessage, BlockRangeScanner, BlockRangeScannerError, ConnectedBlockRangeScanner,
        MAX_BUFFERED_MESSAGES,
    },
    event_filter::EventFilter,
    event_listener::EventListener,
    types::ScannerMessage,
};
use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    providers::{Provider, RootProvider},
    rpc::types::{Filter, Log},
    sol_types::SolEvent,
    transports::{RpcError, TransportErrorKind, http::reqwest::Url},
};
use thiserror::Error;
use tokio::sync::{
    broadcast::{self, Sender, error::RecvError},
    mpsc,
};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tracing::{error, info, warn};

pub struct EventScanner {
    block_range_scanner: BlockRangeScanner,
}

pub type EventScannerMessage = ScannerMessage<Vec<Log>, EventScannerError>;

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

impl<'a, T: SolEvent + 'a> PartialEq<Vec<T>> for EventScannerMessage {
    fn eq(&self, other: &Vec<T>) -> bool {
        self.eq(&other.as_slice())
    }
}

impl<'a, T: SolEvent + 'a> PartialEq<&Vec<T>> for EventScannerMessage {
    fn eq(&self, other: &&Vec<T>) -> bool {
        self.eq(&other.as_slice())
    }
}

impl<'a, T: SolEvent + 'a, const N: usize> PartialEq<&[T; N]> for EventScannerMessage {
    fn eq(&self, other: &&[T; N]) -> bool {
        self.eq(&other.as_slice())
    }
}

impl<'a, T: SolEvent + 'a> PartialEq<&[T]> for EventScannerMessage {
    fn eq(&self, other: &&[T]) -> bool {
        if let EventScannerMessage::Data(logs) = self {
            logs.iter().map(|l| l.data().clone()).eq(other.into_iter().map(|e| e.encode_log_data()))
        } else {
            false
        }
    }
}

#[derive(Debug)]
pub struct LogMetadata<E: SolEvent> {
    pub event: E,
    pub address: alloy::primitives::Address,
    pub tx_hash: alloy::primitives::B256,
}

impl<'a, E: SolEvent + 'a> PartialEq<Vec<LogMetadata<E>>> for EventScannerMessage {
    fn eq(&self, other: &Vec<LogMetadata<E>>) -> bool {
        self.eq(&other.as_slice())
    }
}

impl<'a, E: SolEvent + 'a> PartialEq<&Vec<LogMetadata<E>>> for EventScannerMessage {
    fn eq(&self, other: &&Vec<LogMetadata<E>>) -> bool {
        self.eq(&other.as_slice())
    }
}

impl<'a, E: SolEvent + 'a, const N: usize> PartialEq<&[LogMetadata<E>; N]> for EventScannerMessage {
    fn eq(&self, other: &&[LogMetadata<E>; N]) -> bool {
        self.eq(&other.as_slice())
    }
}

impl<'a, E: SolEvent + 'a> PartialEq<&[LogMetadata<E>]> for EventScannerMessage {
    fn eq(&self, other: &&[LogMetadata<E>]) -> bool {
        if let EventScannerMessage::Data(logs) = self {
            let log_data = logs
                .iter()
                .map(|l| {
                    let address = l.address();
                    let tx_hash = l.transaction_hash.unwrap();
                    (l.inner.data.clone(), address, tx_hash)
                })
                .collect::<Vec<_>>();
            let expected = other
                .into_iter()
                .map(|e| (e.event.encode_log_data(), e.address, e.tx_hash))
                .collect::<Vec<_>>();
            log_data == expected
        } else {
            false
        }
    }
}

#[derive(Error, Debug, Clone)]
pub enum EventScannerError {
    #[error("Block range scanner error: {0}")]
    BlockRangeScanner(#[from] BlockRangeScannerError),
    #[error("Provider error: {0}")]
    Provider(Arc<RpcError<TransportErrorKind>>),
}

impl From<RpcError<TransportErrorKind>> for EventScannerError {
    fn from(e: RpcError<TransportErrorKind>) -> Self {
        EventScannerError::Provider(Arc::new(e))
    }
}

impl From<RpcError<TransportErrorKind>> for EventScannerMessage {
    fn from(e: RpcError<TransportErrorKind>) -> Self {
        EventScannerMessage::Error(e.into())
    }
}

impl From<BlockRangeScannerError> for EventScannerMessage {
    fn from(e: BlockRangeScannerError) -> Self {
        EventScannerMessage::Error(e.into())
    }
}

impl Default for EventScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl EventScanner {
    #[must_use]
    /// Creates a new builder with default block scanner and callback strategy.
    pub fn new() -> Self {
        Self { block_range_scanner: BlockRangeScanner::new() }
    }

    /// Configures how many blocks are read per epoch during a historical sync.
    #[must_use]
    pub fn with_blocks_read_per_epoch(mut self, blocks_read_per_epoch: usize) -> Self {
        self.block_range_scanner =
            self.block_range_scanner.with_blocks_read_per_epoch(blocks_read_per_epoch);
        self
    }

    /// Sets the depth to rewind when a reorg is detected.
    #[must_use]
    pub fn with_reorg_rewind_depth(mut self, reorg_rewind_depth: u64) -> Self {
        self.block_range_scanner =
            self.block_range_scanner.with_reorg_rewind_depth(reorg_rewind_depth);
        self
    }

    /// Configures how many confirmations are required before processing a block (used for reorgs).
    #[must_use]
    pub fn with_block_confirmations(mut self, block_confirmations: u64) -> Self {
        self.block_range_scanner =
            self.block_range_scanner.with_block_confirmations(block_confirmations);
        self
    }

    /// Connects to the provider via WebSocket
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ws<N: Network>(self, ws_url: Url) -> Result<Client<N>, EventScannerError> {
        let block_range_scanner = self.block_range_scanner.connect_ws(ws_url).await?;
        let event_scanner =
            ConnectedEventScanner { block_range_scanner, event_listeners: Vec::default() };
        Ok(Client { event_scanner })
    }

    /// Connects to the provider via IPC
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: impl Into<String>,
    ) -> Result<Client<N>, EventScannerError> {
        let block_range_scanner = self.block_range_scanner.connect_ipc(ipc_path.into()).await?;
        let event_scanner =
            ConnectedEventScanner { block_range_scanner, event_listeners: Vec::default() };
        Ok(Client { event_scanner })
    }

    /// Connects to an existing provider
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub fn connect_provider<N: Network>(
        self,
        provider: RootProvider<N>,
    ) -> Result<Client<N>, EventScannerError> {
        let block_range_scanner = self.block_range_scanner.connect_provider(provider)?;
        let event_scanner =
            ConnectedEventScanner { block_range_scanner, event_listeners: Vec::default() };
        Ok(Client { event_scanner })
    }
}

pub struct ConnectedEventScanner<N: Network> {
    block_range_scanner: ConnectedBlockRangeScanner<N>,
    event_listeners: Vec<EventListener>,
}

impl<N: Network> ConnectedEventScanner<N> {
    /// Starts the scanner
    ///
    /// # Errors
    ///
    /// Returns an error if the scanner fails to start
    pub async fn start(
        &self,
        start_height: BlockNumberOrTag,
        end_height: Option<BlockNumberOrTag>,
    ) -> Result<(), EventScannerError> {
        let client = self.block_range_scanner.run()?;
        let mut stream = if let Some(end_height) = end_height {
            client.stream_historical(start_height, end_height).await?
        } else if matches!(start_height, BlockNumberOrTag::Latest) {
            client.stream_live().await?
        } else {
            client.stream_from(start_height).await?
        };

        let (range_tx, _) = broadcast::channel::<BlockRangeMessage>(1024);

        self.spawn_log_consumers(&range_tx);

        while let Some(message) = stream.next().await {
            if let Err(err) = range_tx.send(message) {
                error!(error = %err, "No receivers, stopping broadcast");
                break;
            }
        }

        Ok(())
    }

    /// Scans the latest blocks and returns the specified number of events.
    ///
    /// # Errors
    ///
    /// Returns an error if the scanner fails to start
    pub async fn scan_latest(
        self,
        count: usize,
        start_height: BlockNumberOrTag,
        end_height: BlockNumberOrTag,
    ) -> Result<(), EventScannerError> {
        let client = self.block_range_scanner.run()?;
        let mut stream = client.rewind(start_height, end_height).await?;

        let (range_tx, _) = broadcast::channel::<BlockRangeMessage>(1024);

        self.spawn_latest_log_consumers(&range_tx, count);

        while let Some(message) = stream.next().await {
            if let Err(err) = range_tx.send(message) {
                error!(error = %err, "No receivers, stopping broadcast");
                break;
            }
        }

        Ok(())
    }

    fn spawn_log_consumers(&self, range_tx: &Sender<BlockRangeMessage>) {
        for listener in &self.event_listeners {
            let provider = self.block_range_scanner.provider().clone();
            let filter = listener.filter.clone();
            let log_filter = Filter::from(&filter);
            let sender = listener.sender.clone();
            let mut sub = range_tx.subscribe();

            tokio::spawn(async move {
                loop {
                    match sub.recv().await {
                        Ok(BlockRangeMessage::Data(range)) => {
                            let logs = Self::get_logs(range, &filter, &log_filter, &provider).await;
                            if let Ok(logs) = &logs
                                && logs.is_empty()
                            {
                                continue;
                            }
                            if let Err(send_err) = sender.send(logs.into()).await {
                                warn!(error = %send_err, "Downstream channel closed, stopping stream");
                                break;
                            }
                        }
                        Ok(BlockRangeMessage::Error(e)) => {
                            if let Err(err) = sender.send(ScannerMessage::Error(e.into())).await {
                                warn!(error = %err, "Downstream channel closed, skipping error propagation and stopping stream.");
                                break;
                            }
                        }
                        Ok(BlockRangeMessage::Status(status)) => {
                            if let Err(err) = sender.send(ScannerMessage::Status(status)).await {
                                warn!(error = %err, "Downstream channel closed, skipping sending info to receiver stream and stopping stream.");
                                break;
                            }
                        }
                        Err(RecvError::Closed) => {
                            warn!("No block ranges to receive, stopping stream.");
                            break;
                        }
                        Err(RecvError::Lagged(_)) => {}
                    }
                }
            });
        }
    }

    fn spawn_latest_log_consumers(&self, range_tx: &Sender<BlockRangeMessage>, count: usize) {
        for listener in &self.event_listeners {
            let provider = self.block_range_scanner.provider().clone();
            let filter = listener.filter.clone();
            let log_filter = Filter::from(&filter);
            let sender = listener.sender.clone();
            let mut sub = range_tx.subscribe();

            tokio::spawn(async move {
                let mut events = Vec::with_capacity(count);
                loop {
                    match sub.recv().await {
                        Ok(BlockRangeMessage::Data(range)) => {
                            match Self::get_logs(range, &filter, &log_filter, &provider).await {
                                Ok(logs) => {
                                    if logs.is_empty() {
                                        continue;
                                    }
                                    // SAFETY: events.len() <= count
                                    let take = count - events.len();
                                    let logs_rev = logs.into_iter().rev().take(take);
                                    events.extend(logs_rev);
                                    if events.len() == count {
                                        break;
                                    }
                                }
                                Err(e) => {
                                    if let Err(send_err) = sender.send(e.into()).await {
                                        error!(error = %send_err, "Downstream channel closed, skip log result propagation and stopping stream");
                                        break;
                                    }
                                }
                            }
                        }
                        Ok(BlockRangeMessage::Error(e)) => {
                            if let Err(err) = sender.send(e.into()).await {
                                warn!(error = %err, "Downstream channel closed, skipping error propagation and stopping streaming.");
                                break;
                            }
                        }
                        Ok(BlockRangeMessage::Status(status)) => {
                            if let Err(err) = sender.send(status.into()).await {
                                warn!(error = %err, "Downstream channel closed, skipping sending info to receiver stream and stopping streaming.");
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

                info!(event_count = events.len(), "Collected events");

                // we collected events in reverse, so put them in correct order before streaming
                events.reverse();

                if let Err(e) = sender.send(events.into()).await {
                    warn!(error = %e, "Downstream channel closed, skipping sending info to receiver stream.");
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

    fn add_event_listener(&mut self, event_listener: EventListener) {
        self.event_listeners.push(event_listener);
    }
}

pub struct Client<N: Network> {
    event_scanner: ConnectedEventScanner<N>,
}

impl<N: Network> Client<N> {
    pub fn create_event_stream(
        &mut self,
        event_filter: EventFilter,
    ) -> ReceiverStream<EventScannerMessage> {
        let (sender, receiver) = mpsc::channel::<EventScannerMessage>(MAX_BUFFERED_MESSAGES);

        self.event_scanner.add_event_listener(EventListener { filter: event_filter, sender });

        ReceiverStream::new(receiver)
    }

    /// Starts the scanner
    ///
    /// # Errors
    ///
    /// Returns an error if the scanner fails to start
    pub async fn start_scanner(
        self,
        start_height: BlockNumberOrTag,
        end_height: Option<BlockNumberOrTag>,
    ) -> Result<(), EventScannerError> {
        self.event_scanner.start(start_height, end_height).await
    }

    /// Scans the latest `count` events.
    ///
    /// # Errors
    ///
    /// Returns an error if the scanner fails to scan
    pub async fn scan_latest(self, count: usize) -> Result<(), EventScannerError> {
        self.event_scanner
            .scan_latest(count, BlockNumberOrTag::Earliest, BlockNumberOrTag::Latest)
            .await
    }

    /// Scans the latest `count` events in a given block range.
    ///
    /// # Errors
    ///
    /// Returns an error if the scanner fails to scan
    pub async fn scan_latest_in_range(
        self,
        count: usize,
        start_height: BlockNumberOrTag,
        end_height: BlockNumberOrTag,
    ) -> Result<(), EventScannerError> {
        self.event_scanner.scan_latest(count, start_height, end_height).await
    }
}
