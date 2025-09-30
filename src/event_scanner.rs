use std::sync::Arc;

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

pub type EventScannerMessage = ScannerMessage<Vec<Log>, EventScannerError>;

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

        let (range_tx, _) = broadcast::channel::<BlockRangeMessage>(1024);

        self.spawn_log_consumers(&range_tx);

        while let Some(message) = stream.next().await {
            if let Err(send_err) = range_tx.send(message) {
                error!(error = %send_err, "failed to send error");
            }
        }

        Ok(())
    }

    fn spawn_log_consumers(&self, range_tx: &Sender<BlockRangeMessage>) {
        for listener in &self.event_listeners {
            let provider = self.block_range_scanner.provider().clone();
            let filter = listener.filter.clone();
            let sender = listener.sender.clone();
            let mut sub = range_tx.subscribe();

            tokio::spawn(async move {
                loop {
                    match sub.recv().await {
                        Ok(BlockRangeMessage::Data(range)) => {
                            let (from_block, to_block) = (*range.start(), *range.end());

                            let mut log_filter =
                                Filter::new().from_block(from_block).to_block(to_block);

                            if let Some(contract_address) = filter.contract_address {
                                log_filter = log_filter.address(contract_address);
                            }

                            if let Some(ref event_signature) = filter.event {
                                log_filter = log_filter.event(event_signature.as_str());
                            }

                            let contract_display = filter.contract_address.map_or_else(
                                || "all contracts".to_string(),
                                |addr| format!("{addr:?}"),
                            );
                            let event_display = filter.event.as_deref().map_or("all events", |s| s);

                            match provider.get_logs(&log_filter).await {
                                Ok(logs) => {
                                    if logs.is_empty() {
                                        continue;
                                    }

                                    info!(
                                        contract = %contract_display,
                                        event = %event_display,
                                        log_count = logs.len(),
                                        from_block,
                                        to_block,
                                        "found logs for event in block range"
                                    );

                                    if let Err(e) =
                                        sender.send(EventScannerMessage::Data(logs)).await
                                    {
                                        error!(contract = %contract_display, event = %event_display, error = %e, "failed to enqueue log for processing");
                                    }
                                }
                                Err(e) => {
                                    error!(
                                        contract = %contract_display,
                                        event = %event_display,
                                        error = %e,
                                        from_block,
                                        to_block,
                                        "failed to get logs for block range"
                                    );

                                    if let Err(send_err) = sender
                                        .send(EventScannerMessage::Error(EventScannerError::from(
                                            e,
                                        )))
                                        .await
                                    {
                                        error!(event = %event_display, error = %send_err, "failed to enqueue error for processing");
                                    }
                                }
                            }
                        }
                        Ok(BlockRangeMessage::Error(e)) => {
                            warn!(error = %e, "block range scanner error");
                            if let Err(send_err) = sender
                                .send(EventScannerMessage::Error(
                                    EventScannerError::BlockRangeScanner(e),
                                ))
                                .await
                            {
                                error!(error = %send_err, "failed to send error to broadcast channel");
                            }
                        }
                        Ok(BlockRangeMessage::Info(info)) => {
                            info!("Received info from block range scanner: {:?}", info);
                            if let Err(send_err) = sender.send(ScannerMessage::Info(info)).await {
                                error!(error = %send_err, "failed to send error to");
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
}
