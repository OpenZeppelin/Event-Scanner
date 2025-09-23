use crate::{
    block_range_scanner::{self, BlockRangeScanner, ConnectedBlockRangeScanner},
    types::EventFilter,
};
use alloy::{
    eips::BlockNumberOrTag, network::Network, providers::Provider, rpc::types::Filter,
    transports::http::reqwest::Url,
};
use tokio_stream::StreamExt;
use tracing::{error, info};

pub struct EventScanner {
    block_range_scanner: BlockRangeScanner,
    tracked_events: Vec<EventFilter>,
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
        Self { block_range_scanner: BlockRangeScanner::new(), tracked_events: Vec::default() }
    }

    /// Registers a single event filter for scanning.
    #[must_use]
    pub fn with_event_filter(mut self, filter: EventFilter) -> Self {
        self.tracked_events.push(filter);
        self
    }

    /// Registers a collection of event filters for scanning.
    #[must_use]
    pub fn with_event_filters(mut self, filters: Vec<EventFilter>) -> Self {
        self.tracked_events.extend(filters);
        self
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
    ) -> Result<ConnectedEventScanner<N>, block_range_scanner::Error> {
        let block_range_scanner = self.block_range_scanner.connect_ws(ws_url).await?;
        Ok(ConnectedEventScanner { block_range_scanner, tracked_events: self.tracked_events })
    }

    /// Connects to the provider via IPC
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: impl Into<String>,
    ) -> Result<ConnectedEventScanner<N>, block_range_scanner::Error> {
        let block_range_scanner = self.block_range_scanner.connect_ipc(ipc_path.into()).await?;
        Ok(ConnectedEventScanner { block_range_scanner, tracked_events: self.tracked_events })
    }
}

pub struct ConnectedEventScanner<N: Network> {
    block_range_scanner: ConnectedBlockRangeScanner<N>,
    tracked_events: Vec<EventFilter>,
}

impl<N: Network> ConnectedEventScanner<N> {
    /// Starts the scanner
    ///
    /// # Errors
    ///
    /// Returns an error if the scanner fails to start
    pub async fn start(
        &mut self,
        start_height: BlockNumberOrTag,
        end_height: Option<BlockNumberOrTag>,
    ) -> anyhow::Result<()> {
        let client = self.block_range_scanner.run()?;
        let mut stream = client.subscribe(start_height, end_height).await?;

        while let Some(range) = stream.next().await {
            match range {
                Ok(range) => {
                    let from_block = range.start;
                    let to_block = range.end;
                    info!(from_block, to_block, "processing block range");
                    self.process_block_range(from_block, to_block).await?;
                }
                Err(e) => {
                    error!(error = %e, "failed to get block range");
                }
            }
        }

        Ok(())
    }

    /// Fetches logs for the supplied block range and forwards them to the callback channels.
    async fn process_block_range(&self, from_block: u64, to_block: u64) -> anyhow::Result<()> {
        for event_filter in &self.tracked_events {
            let filter = Filter::new()
                .address(event_filter.contract_address)
                .event(event_filter.event.as_str())
                .from_block(from_block)
                .to_block(to_block);

            match self.block_range_scanner.provider().get_logs(&filter).await {
                Ok(logs) => {
                    if logs.is_empty() {
                        continue;
                    }
                    info!(
                        contract = ?event_filter.contract_address,
                        event = %event_filter.event,
                        log_count = logs.len(),
                        from_block,
                        to_block,
                        "found logs for event in block range"
                    );

                    if let Err(e) = event_filter.sender.send(Ok(logs)).await {
                        error!(event = %event_filter.event, error = %e, "failed to enqueue logs for processing");
                    }
                }
                Err(e) => {
                    error!(
                        contract = ?event_filter.contract_address,
                        event = %event_filter.event,
                        error = %e,
                        from_block,
                        to_block,
                        "failed to get logs for block range"
                    );

                    if let Err(e) = event_filter.sender.send(Err(e.into())).await {
                        error!(event = %event_filter.event, error = %e, "failed to enqueue error for processing");
                    }
                }
            }
        }

        Ok(())
    }
}
