use alloy::{
    consensus::BlockHeader,
    eips::BlockNumberOrTag,
    network::{BlockResponse, Network},
    providers::{Provider, RootProvider},
    transports::{TransportResult, http::reqwest::Url},
};

use tokio::sync::mpsc;
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tracing::info;

use crate::{
    ScannerError, ScannerStatus,
    block_range_scanner::{
        BlockRangeScanner, ConnectedBlockRangeScanner, DEFAULT_BLOCK_CONFIRMATIONS,
        MAX_BUFFERED_MESSAGES, Message as BlockRangeMessage,
    },
    event_scanner::{
        filter::EventFilter,
        listener::EventListener,
        message::Message,
        scanner::common::{ConsumerMode, handle_stream},
    },
};

pub struct SyncFromLatestScannerBuilder {
    block_range_scanner: BlockRangeScanner,
    latest_events_count: usize,
    block_confirmations: u64,
}

pub struct SyncFromLatestEventScanner<N: Network> {
    config: SyncFromLatestScannerBuilder,
    block_range_scanner: ConnectedBlockRangeScanner<N>,
    listeners: Vec<EventListener>,
}

impl SyncFromLatestScannerBuilder {
    #[must_use]
    pub(crate) fn new(count: usize) -> Self {
        Self {
            block_range_scanner: BlockRangeScanner::new(),
            latest_events_count: count,
            block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS,
        }
    }

    #[must_use]
    pub fn block_confirmations(mut self, count: u64) -> Self {
        self.block_confirmations = count;
        self
    }

    /// Connects to the provider via WebSocket.
    ///
    /// Final builder method: consumes the builder and returns the built
    /// [`SyncFromLatestEventScanner`].
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ws<N: Network>(
        self,
        ws_url: Url,
    ) -> TransportResult<SyncFromLatestEventScanner<N>> {
        let block_range_scanner = self.block_range_scanner.connect_ws::<N>(ws_url).await?;
        Ok(SyncFromLatestEventScanner { config: self, block_range_scanner, listeners: Vec::new() })
    }

    /// Connects to the provider via IPC.
    ///
    /// Final builder method: consumes the builder and returns the built
    /// [`SyncFromLatestEventScanner`].
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<SyncFromLatestEventScanner<N>> {
        let block_range_scanner = self.block_range_scanner.connect_ipc::<N>(ipc_path).await?;
        Ok(SyncFromLatestEventScanner { config: self, block_range_scanner, listeners: Vec::new() })
    }

    /// Connects to an existing provider.
    ///
    /// Final builder method: consumes the builder and returns the built
    /// [`SyncFromLatestEventScanner`].
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    #[must_use]
    pub fn connect<N: Network>(self, provider: RootProvider<N>) -> SyncFromLatestEventScanner<N> {
        let block_range_scanner = self.block_range_scanner.connect::<N>(provider);
        SyncFromLatestEventScanner { config: self, block_range_scanner, listeners: Vec::new() }
    }
}

impl<N: Network> SyncFromLatestEventScanner<N> {
    #[must_use]
    pub fn subscribe(&mut self, filter: EventFilter) -> ReceiverStream<Message> {
        let (sender, receiver) = mpsc::channel::<Message>(MAX_BUFFERED_MESSAGES);
        self.listeners.push(EventListener { filter, sender });
        ReceiverStream::new(receiver)
    }

    /// Starts the scanner.
    ///
    /// # Errors
    ///
    /// Returns an error if the scanner fails to start.
    #[allow(clippy::missing_panics_doc)]
    pub async fn start(self) -> Result<(), ScannerError> {
        let count = self.config.latest_events_count;
        let provider = self.block_range_scanner.provider().clone();
        let listeners = self.listeners.clone();

        info!(count = count, "Starting scanner, mode: fetch latest events and switch to live");

        let client = self.block_range_scanner.run()?;

        // Fetch the latest block number.
        // This is used to determine the starting point for the rewind stream and the live
        // stream. We do this before starting the streams to avoid a race condition
        // where the latest block changes while we're setting up the streams.
        let latest_block = provider
            .get_block_by_number(BlockNumberOrTag::Latest)
            .await?
            .ok_or(ScannerError::BlockNotFound(BlockNumberOrTag::Latest))?
            .header()
            .number();

        // Setup rewind and live streams to run in parallel.
        let rewind_stream = client.rewind(BlockNumberOrTag::Earliest, latest_block).await?;
        // We actually rely on the sync mode for the live stream, to
        // ensure that we don't miss any events in case a new block was minted while
        // we were setting up the streams or a reorg happens.
        let sync_stream =
            client.stream_from(latest_block + 1, self.config.block_confirmations).await?;

        // Start streaming...
        tokio::spawn(async move {
            // Since both rewind and live log consumers are ultimately streaming to the same
            // channel, we must ensure that all latest events are streamed before
            // consuming the live stream, otherwise the log consumers may send events out
            // of order.
            handle_stream(
                rewind_stream,
                &provider,
                &listeners,
                ConsumerMode::CollectLatest { count },
            )
            .await;

            // Notify the client that we're now streaming live.
            info!("Switching to live stream");

            // Use a one-off channel for the notification.
            let (tx, rx) = mpsc::channel::<BlockRangeMessage>(1);
            let stream = ReceiverStream::new(rx);
            tx.send(BlockRangeMessage::Status(ScannerStatus::SwitchingToLive))
                .await
                .expect("receiver exists");

            // close the channel to stop the stream
            drop(tx);

            let sync_stream = stream.chain(sync_stream);

            // Start the live (sync) stream.
            handle_stream(sync_stream, &provider, &listeners, ConsumerMode::Stream).await;
        });

        Ok(())
    }
}
