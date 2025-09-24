use crate::{
    block_range_scanner::{
        self, BlockRangeScanner, ConnectedBlockRangeScanner, MAX_BUFFERED_MESSAGES,
    },
    types::{EventFilter, EventListener},
};
use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    providers::Provider,
    rpc::types::{Filter, Log},
    transports::http::reqwest::Url,
};
use tokio::sync::mpsc;
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tracing::{error, info};

pub struct EventScanner {
    block_range_scanner: BlockRangeScanner,
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
    pub async fn connect_ws<N: Network>(
        self,
        ws_url: Url,
    ) -> Result<Client<N>, block_range_scanner::Error> {
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
    ) -> Result<Client<N>, block_range_scanner::Error> {
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
    ) -> Result<(), block_range_scanner::Error> {
        let client = self.block_range_scanner.run()?;
        let mut stream = client.subscribe(start_height, end_height).await?;

        while let Some(range) = stream.next().await {
            match range {
                Ok(range) => {
                    let from_block = range.start;
                    let to_block = range.end;
                    info!(from_block, to_block, "processing block range");
                    self.process_block_range(from_block, to_block).await;
                }
                Err(e) => {
                    error!(error = %e, "failed to get block range");
                }
            }
        }

        Ok(())
    }

    /// Fetches logs for the supplied block range and forwards them to the callback channels.
    async fn process_block_range(&self, from_block: u64, to_block: u64) {
        for listener in &self.event_listeners {
            let filter = Filter::new()
                .address(listener.filter.contract_address)
                .event(listener.filter.event.as_str())
                .from_block(from_block)
                .to_block(to_block);

            match self.block_range_scanner.provider().get_logs(&filter).await {
                Ok(logs) => {
                    if logs.is_empty() {
                        continue;
                    }
                    info!(
                        contract = ?listener.filter.contract_address,
                        event = %listener.filter.event,
                        log_count = logs.len(),
                        from_block,
                        to_block,
                        "found logs for event in block range"
                    );

                    if let Err(e) = listener.sender.send(Ok(logs)).await {
                        error!(event = %listener.filter.event, error = %e, "failed to enqueue logs for processing");
                    }
                }
                Err(e) => {
                    error!(
                        contract = ?listener.filter.contract_address,
                        event = %listener.filter.event,
                        error = %e,
                        from_block,
                        to_block,
                        "failed to get logs for block range"
                    );

                    if let Err(e) = listener.sender.send(Err(e.into())).await {
                        error!(event = %listener.filter.event, error = %e, "failed to enqueue error for processing");
                    }
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
    pub fn subscribe(
        &mut self,
        event_filter: EventFilter,
    ) -> ReceiverStream<Result<Vec<Log>, block_range_scanner::Error>> {
        let (sender, receiver) = mpsc::channel(MAX_BUFFERED_MESSAGES);

        self.event_scanner.add_event_listener(EventListener { filter: event_filter, sender });

        ReceiverStream::new(receiver)
    }

    pub async fn start_scanner(
        self,
        start_height: BlockNumberOrTag,
        end_height: Option<BlockNumberOrTag>,
    ) -> Result<(), block_range_scanner::Error> {
        self.event_scanner.start(start_height, end_height).await
    }
}
