use std::{ops::RangeInclusive, sync::Arc};

use crate::{
    block_range_scanner::{
        BlockRangeMessage, BlockRangeScanner, BlockRangeScannerError, ConnectedBlockRangeScanner,
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
    transports::{RpcError, TransportErrorKind, http::reqwest::Url},
};
use thiserror::Error;
use tokio::sync::{
    broadcast::{self, Sender, error::RecvError},
    mpsc,
};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tracing::{error, info};

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

    #[must_use]
    pub fn with_max_block_range(mut self, max_block_range: usize) -> Self {
        self.block_range_scanner =
            self.block_range_scanner.with_max_read_per_epoch(max_block_range);
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
    /// Implementation of stream live
    ///
    /// # Errors
    ///
    /// * `EventScannerMessage::ServiceShutdown` - if the service is already shutting down.
    pub async fn stream_live(
        &self,
        block_confirmations: Option<u64>,
    ) -> Result<(), EventScannerError> {
        let client = self.block_range_scanner.run()?;
        let stream = client.stream_live(block_confirmations).await?;

        self.handle_stream(stream).await;

        Ok(())
    }

    /// Implementation of stream historical
    ///
    /// # Errors
    ///
    /// * `EventScannerMessage::ServiceShutdown` - if the service is already shutting down.
    pub async fn stream_historical(
        &self,
        start_height: BlockNumberOrTag,
        end_height: BlockNumberOrTag,
    ) -> Result<(), EventScannerError> {
        let client = self.block_range_scanner.run()?;
        let stream = client.stream_historical(start_height, end_height).await?;

        self.handle_stream(stream).await;

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
        block_confirmations: Option<u64>,
    ) -> Result<(), EventScannerError> {
        let client = self.block_range_scanner.run()?;
        let stream = client.stream_from(start_height, block_confirmations).await?;

        self.handle_stream(stream).await;

        Ok(())
    }

    async fn handle_stream(
        &self,
        mut stream: ReceiverStream<ScannerMessage<RangeInclusive<u64>, BlockRangeScannerError>>,
    ) {
        let (range_tx, _) = broadcast::channel::<BlockRangeMessage>(1024);
        self.spawn_log_consumers(&range_tx);

        while let Some(message) = stream.next().await {
            if let Err(err) = range_tx.send(message) {
                error!(error = %err, "Broadcast channel closed, could not send BlockRangeMessage. Stopped streaming block ranges.");
                break;
            }
        }
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
                            Self::process_range(range, &filter, &log_filter, &provider, &sender)
                                .await;
                        }
                        Ok(BlockRangeMessage::Error(e)) => {
                            if let Err(err) =
                                sender.send(EventScannerMessage::Error(e.into())).await
                            {
                                error!(error = %err, "Downstream channel closed, skipping error propagation and stopping streaming.");
                                break;
                            }
                        }
                        Ok(BlockRangeMessage::Status(status)) => {
                            if let Err(err) = sender.send(EventScannerMessage::Status(status)).await
                            {
                                error!(error = %err, "Downstream channel closed, skipping sending info to receiver stream and stopping streaming.");
                                break;
                            }
                        }
                        Err(RecvError::Closed) => {
                            error!("No block ranges to receive, stopping streaming.");
                            break;
                        }
                        Err(RecvError::Lagged(_)) => {}
                    }
                }
            });
        }
    }

    async fn process_range(
        range: RangeInclusive<u64>,
        event_filter: &EventFilter,
        log_filter: &Filter,
        provider: &RootProvider<N>,
        sender: &mpsc::Sender<EventScannerMessage>,
    ) {
        let (from_block, to_block) = (*range.start(), *range.end());

        let log_filter = log_filter.clone().from_block(from_block).to_block(to_block);

        match provider.get_logs(&log_filter).await {
            Ok(logs) => {
                if logs.is_empty() {
                    return;
                }

                info!(
                    filter = %event_filter,
                    log_count = logs.len(),
                    from_block,
                    to_block,
                    "found logs for event in block range"
                );

                if let Err(e) = sender.send(EventScannerMessage::Data(logs)).await {
                    error!(filter = %event_filter, error = %e, "failed to enqueue log for processing");
                }
            }
            Err(e) => {
                error!(
                    filter = %event_filter,
                    error = %e,
                    from_block,
                    to_block,
                    "failed to get logs for block range"
                );

                if let Err(send_err) = sender.send(EventScannerMessage::Error(e.into())).await {
                    error!(filter = %event_filter, error = %send_err, "failed to enqueue error for processing");
                }
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

    /// Streams live events starting from the latest block.
    ///
    /// # Arguments
    ///
    /// * `block_confirmation`: Number of confirmations to apply once in live mode.
    /// # Errors
    ///
    /// * `EventScannerMessage::ServiceShutdown` - if the service is already shutting down.
    pub async fn stream_live(
        self,
        block_confirmations: Option<u64>,
    ) -> Result<(), EventScannerError> {
        self.event_scanner.stream_live(block_confirmations).await
    }

    /// Streams a batch of historical evnets from `start_height` to `end_height`.
    ///
    /// # Arguments
    ///
    /// * `start_height` - The starting block number or tag.
    /// * `end_height` - The ending block number or tag.
    ///
    /// # Errors
    ///
    /// * `EventScannerMessage::ServiceShutdown` - if the service is already shutting down.
    pub async fn stream_historical(
        self,
        start_height: BlockNumberOrTag,
        end_height: BlockNumberOrTag,
    ) -> Result<(), EventScannerError> {
        self.event_scanner.stream_historical(start_height, end_height).await
    }

    /// Streams events starting from `start_height` and transitions to live mode.
    ///
    /// # Arguments
    ///
    /// * `start_height` - The starting block number or tag.
    /// * `block_confirmations` - Number of confirmations to apply once in live mode.
    ///
    /// # Errors
    ///
    /// * `EventScannerMessage::ServiceShutdown` - if the service is already shutting down.
    pub async fn stream_from(
        self,
        start_height: BlockNumberOrTag,
        block_confirmations: Option<u64>,
    ) -> Result<(), EventScannerError> {
        self.event_scanner.stream_from(start_height, block_confirmations).await
    }
}
