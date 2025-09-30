use std::{ops::RangeInclusive, sync::Arc};

use crate::{
    block_range_scanner::{
        BlockRangeScanner, BlockRangeScannerError, ConnectedBlockRangeScanner,
        MAX_BUFFERED_MESSAGES,
    },
    event_filter::EventFilter,
    event_listener::EventListener,
};
use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    providers::Provider,
    rpc::types::{Filter, Log},
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

        let (range_tx, _) =
            broadcast::channel::<Result<RangeInclusive<u64>, Arc<EventScannerError>>>(1024);

        self.spawn_log_consumers(&range_tx);

        while let Some(range) = stream.next().await {
            match range {
                Ok(range) => {
                    info!(?range, "processing block range");
                    if let Err(e) = range_tx.send(Ok(range)) {
                        error!(error = %e, "failed to send block range to broadcast channel");
                        break;
                    }
                }
                Err(e) => {
                    warn!(error = %e, "block range scanner error");
                    // Propagate the error to the range channel
                    if let Err(send_err) = range_tx.send(Err(Arc::new(e.into()))) {
                        error!(error = %send_err, "failed to send error to broadcast channel");
                    }
                }
            }
        }

        Ok(())
    }

    fn spawn_log_consumers(
        &self,
        range_tx: &Sender<Result<RangeInclusive<u64>, Arc<EventScannerError>>>,
    ) {
        for listener in &self.event_listeners {
            let provider = self.block_range_scanner.provider().clone();
            let filter = listener.filter.clone();
            let sender = listener.sender.clone();
            let mut sub = range_tx.subscribe();

            tokio::spawn(async move {
                loop {
                    match sub.recv().await {
                        Ok(Ok(range)) => {
                            let (from_block, to_block) = (*range.start(), *range.end());

                            let mut log_filter = Filter::new()
                                .from_block(from_block)
                                .to_block(to_block)
                                .events(filter.events.clone());

                            if let Some(contract_address) = filter.contract_address {
                                log_filter = log_filter.address(contract_address);
                            }

                            match provider.get_logs(&log_filter).await {
                                Ok(logs) => {
                                    if logs.is_empty() {
                                        continue;
                                    }

                                    info!(
                                        filter = %filter,
                                        log_count = logs.len(),
                                        from_block,
                                        to_block,
                                        "found logs for event in block range"
                                    );

                                    if let Err(e) = sender.send(Ok(logs)).await {
                                        error!(filter = %filter, error = %e, "failed to enqueue log for processing");
                                    }
                                }
                                Err(e) => {
                                    error!(
                                        filter = %filter,
                                        error = %e,
                                        from_block,
                                        to_block,
                                        "failed to get logs for block range"
                                    );

                                    if let Err(send_err) =
                                        sender.send(Err(Arc::new(EventScannerError::from(e)))).await
                                    {
                                        warn!(filter = %filter, error = %send_err, "Downstream channel closed, error could not be enqueued. Stopping event stream.");
                                        break;
                                    }
                                }
                            }
                        }
                        Ok(Err(e)) => {
                            error!("Received error from block range scanner: {}", e);
                            // send error back to event stream
                            if let Err(send_err) = sender.send(Err(e)).await {
                                error!(
                                    error = %send_err,
                                    "Failed to forward block range scanner error to event listener"
                                );
                            }
                        }
                        // TODO: What happens if the broadcast channel is closed?
                        Err(RecvError::Closed) => break,
                        Err(RecvError::Lagged(_)) => {}
                    }
                }
            });
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
    ) -> ReceiverStream<Result<Vec<Log>, Arc<EventScannerError>>> {
        let (sender, receiver) =
            mpsc::channel::<Result<Vec<Log>, Arc<EventScannerError>>>(MAX_BUFFERED_MESSAGES);

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
}
