use std::{ops::RangeInclusive, sync::Arc};

use crate::{
    block_range_scanner::{
        BlockRangeMessage, BlockRangeScanner, BlockRangeScannerError, ConnectedBlockRangeScanner,
        MAX_BUFFERED_MESSAGES,
    },
    event_filter::EventFilter,
    event_listener::EventListener,
    types::{ScannerMessage, ScannerStatus},
};
use alloy::{
    consensus::BlockHeader,
    eips::BlockNumberOrTag,
    network::{BlockResponse, Network},
    providers::{Provider, RootProvider},
    rpc::types::{Filter, Log},
    sol_types::SolEvent,
    transports::{RpcError, TransportErrorKind, http::reqwest::Url},
};
use thiserror::Error;
use tokio::{
    sync::{
        broadcast::{self, Sender, error::RecvError},
        mpsc,
    },
    task::JoinSet,
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

#[derive(Copy, Clone, Debug)]
enum ConsumerMode {
    Stream,
    CollectLatest { count: usize },
}

pub struct ConnectedEventScanner<N: Network> {
    block_range_scanner: ConnectedBlockRangeScanner<N>,
    event_listeners: Vec<EventListener>,
}

impl<N: Network> ConnectedEventScanner<N> {
    /// Starts the scanner
    ///
    /// Selects live, historical, or historical→live mode based on `start_height`/`end_height`.
    ///
    /// # Arguments
    ///
    /// * `start_height` - Start block (tag or number).
    /// * `end_height` - Optional end block (tag or number). If `Some`, a historical scan is
    ///   performed over the inclusive range; if `None`, the scanner either streams live or performs
    ///   historical→live depending on `start_height`.
    ///
    /// # Reorg behavior
    ///
    /// * Historical: verifies chain continuity and if a reorg is detected, rewinds to the
    ///   appropriate post-reorg block, then continues forward.
    /// * Live: on reorg, emits [`ScannerStatus::ReorgDetected`] and adjusts the next block range
    ///   using `with_block_confirmations` to re-emit the confirmed portion.
    /// * Historical → Live: reorgs are handled as per the particular mode the scanner is in
    ///   (historical or live).
    ///
    /// ## ⚠️ Warning: Parallel Reorg Detection (Historical → Live mode)
    ///
    /// When using Historical → Live mode (sync mode via `stream_from`), both the historical and
    /// live phases run in parallel. If a reorg occurs during the historical phase, **both phases
    /// will independently detect and handle the same reorg**. This can result in:
    /// - **Duplicate [`ScannerStatus::ReorgDetected`] messages**: One from the historical phase and
    ///   one from the live phase.
    /// - **Potential duplicate logs**: The live phase may re-emit logs that were already delivered
    ///   by the historical phase if the reorg affects blocks in the overlapping boundary region.
    ///
    /// Applications should be prepared to handle duplicate reorg notifications and implement
    /// deduplication logic if necessary, especially when processing events near the phase
    /// transition boundary.
    ///
    /// Will be handled by: <https://github.com/OpenZeppelin/Event-Scanner/issues/131>
    ///
    /// # Errors
    ///
    /// Returns an error if the scanner fails to start
    ///
    /// [`ScannerStatus::ReorgDetected`]: crate::types::ScannerStatus::ReorgDetected
    pub async fn start<T: Into<BlockNumberOrTag>>(
        &self,
        start_height: T,
        end_height: Option<T>,
    ) -> Result<(), EventScannerError> {
        let client = self.block_range_scanner.run()?;

        let start_height = start_height.into();
        let end_height = end_height.map(Into::into);

        let mut stream = if let Some(end_height) = end_height {
            client.stream_historical(start_height, end_height).await?
        } else if matches!(start_height, BlockNumberOrTag::Latest) {
            client.stream_live().await?
        } else {
            client.stream_from(start_height).await?
        };

        let (range_tx, _) = broadcast::channel::<BlockRangeMessage>(MAX_BUFFERED_MESSAGES);

        self.spawn_log_consumers(&range_tx, ConsumerMode::Stream);

        while let Some(message) = stream.next().await {
            if let Err(err) = range_tx.send(message) {
                error!(error = %err, "No receivers, stopping broadcast");
                break;
            }
        }

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
    /// Returns `EventScannerError` if the scanner fails to start or fetching logs fails.
    ///
    /// [`ScannerStatus::ReorgDetected`]: crate::types::ScannerStatus::ReorgDetected
    pub async fn scan_latest(
        self,
        count: usize,
        start_height: BlockNumberOrTag,
        end_height: BlockNumberOrTag,
    ) -> Result<(), EventScannerError> {
        let client = self.block_range_scanner.run()?;
        let mut stream = client.rewind(start_height, end_height).await?;

        let (range_tx, _) = broadcast::channel::<BlockRangeMessage>(MAX_BUFFERED_MESSAGES);

        self.spawn_log_consumers(&range_tx, ConsumerMode::CollectLatest { count });

        while let Some(message) = stream.next().await {
            if !Self::try_broadcast(&range_tx, message) {
                break;
            }
        }

        Ok(())
    }

    /// Scans the latest `count` matching events per registered listener, then automatically
    /// transitions to live streaming mode.
    ///
    /// This method combines two scanning phases into a single operation:
    /// 1. **Historical rewind phase**: Collects up to `count` most recent events by scanning
    ///    backwards from the current chain tip
    /// 2. **Live streaming phase**: Continuously monitors and streams new events as they arrive
    ///    on-chain
    ///
    /// # Two-Phase Operation
    ///
    /// The method captures the latest block number before starting both phases to establish a
    /// clear boundary. The historical phase scans from `Earliest` to `latest_block`, while the
    /// live phase uses sync mode starting from `latest_block + 1`. This design prevents duplicate
    /// events and handles race conditions where new blocks arrive during setup.
    ///
    /// Between phases, the scanner emits [`ScannerStatus::SwitchingToLive`] to notify listeners
    /// of the transition. As previously mentioned, the live phase internally uses sync mode
    /// (historical → live) to ensure no events are missed if blocks were mined during the
    /// transition or if reorgs occur.
    ///
    /// # Arguments
    ///
    /// * `count` - Maximum number of recent events to collect per listener before switching to
    ///   live.
    ///
    /// # Errors
    ///
    /// Returns `EventScannerError` if the scanner fails to start or fetching logs fails.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use alloy::network::Ethereum;
    /// use event_scanner::{EventFilter, EventScanner, EventScannerMessage};
    /// use tokio_stream::StreamExt;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let ws_url = "ws://localhost:8545".parse()?;
    /// # let contract_address = "0x0000000000000000000000000000000000000000".parse()?;
    /// let mut client = EventScanner::new().connect_ws::<Ethereum>(ws_url).await?;
    ///
    /// let filter = EventFilter::new().with_contract_address(contract_address);
    /// let mut stream = client.create_event_stream(filter);
    ///
    /// // Fetch the latest 10 events, then stream new events continuously
    /// client.scan_latest_then_live(10).await?;
    ///
    /// while let Some(msg) = stream.next().await {
    ///     match msg {
    ///         EventScannerMessage::Data(logs) => {
    ///             println!("Received {} events", logs.len());
    ///         }
    ///         EventScannerMessage::Status(status) => {
    ///             println!("Status: {:?}", status);
    ///         }
    ///         EventScannerMessage::Error(e) => {
    ///             eprintln!("Error: {}", e);
    ///         }
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Edge Cases
    ///
    /// - **No historical events**: If fewer than `count` events exist (or none at all), the method
    ///   returns all available events, then transitions to live streaming normally.
    /// - **Duplicate prevention**: The boundary at `latest_block` ensures events are never
    ///   delivered twice across the phase transition.
    /// - **Race conditions**: Fetching `latest_block` before setting up streams prevents missing
    ///   events that arrive during initialization.
    ///
    /// # Reorg Behavior
    ///
    /// - **Historical rewind phase**: Reverse-ordered rewind over `Earliest..=latest_block`. On
    ///   detecting a reorg, emits [`ScannerStatus::ReorgDetected`], resets the rewind start to the
    ///   new tip, and continues until collectors accumulate `count` logs. Final delivery to
    ///   listeners preserves chronological order.
    /// - **Live streaming phase**: Starts from `latest_block + 1` and respects block confirmations
    ///   configured via [`with_block_confirmations`](Self::with_block_confirmations). On reorg,
    ///   emits [`ScannerStatus::ReorgDetected`], adjusts the next confirmed window (possibly
    ///   re-emitting confirmed portions), and continues streaming.
    ///
    /// ## ⚠️ Warning: Parallel Reorg Detection
    ///
    /// Both phases run in parallel, which means if a reorg occurs during the historical rewind
    /// phase, **both phases will independently detect and handle the same reorg**. This can result
    /// in:
    /// - **Duplicate [`ScannerStatus::ReorgDetected`] messages**: One from the historical phase and
    ///   one from the live phase.
    /// - **Potential duplicate logs**: The live phase may re-emit logs that were already delivered
    ///   by the historical phase if the reorg affects blocks in the overlapping boundary region.
    ///
    /// Applications should be prepared to handle duplicate reorg notifications and implement
    /// deduplication logic if necessary, especially when processing events near the phase
    /// transition boundary.
    ///
    /// Will be handled by: <https://github.com/OpenZeppelin/Event-Scanner/issues/132>
    ///
    /// # Usage Notes
    ///
    /// - Call [`create_event_stream`](Self::create_event_stream) to register listeners **before**
    ///   calling this method, otherwise no events will be delivered.
    /// - The method returns immediately after spawning the scanning task. Events are delivered
    ///   asynchronously through the registered streams.
    /// - The live phase continues indefinitely until the scanner is dropped or an error occurs.
    ///
    /// [`ScannerStatus::ReorgDetected`]: crate::types::ScannerStatus::ReorgDetected
    /// [`ScannerStatus::SwitchingToLive`]: crate::types::ScannerStatus::SwitchingToLive
    pub async fn scan_latest_then_live(self, count: usize) -> Result<(), EventScannerError> {
        info!(count = count, "Starting scanner, mode: fetch latest events and switch to live");

        let client = self.block_range_scanner.run()?;

        // Setup rewind and live stream log consumers.
        let (rewind_tx, _) = broadcast::channel::<BlockRangeMessage>(MAX_BUFFERED_MESSAGES);
        let rewind_log_consumers =
            self.spawn_log_consumers(&rewind_tx, ConsumerMode::CollectLatest { count });

        let (live_tx, _) = broadcast::channel::<BlockRangeMessage>(MAX_BUFFERED_MESSAGES);
        let live_consumers = self.spawn_log_consumers(&live_tx, ConsumerMode::Stream);

        // Fetch the latest block number.
        // This is used to determine the starting point for the rewind stream and the live stream.
        // We do this before starting the streams to avoid a race condition where the latest block
        // changes while we're setting up the streams.
        let latest_block = self
            .block_range_scanner
            .provider()
            .get_block_by_number(BlockNumberOrTag::Latest)
            .await?
            .ok_or_else(|| BlockRangeScannerError::BlockNotFound(BlockNumberOrTag::Latest))?
            .header()
            .number();

        // Setup streams, which run in parallel.
        let mut rewind_stream = client.rewind(BlockNumberOrTag::Earliest, latest_block).await?;

        // We actually rely on the sync mode for the live stream, to ensure that we don't miss any
        // events in case a new block was minted while we were setting up the streams or a reorg
        // happens.
        let mut sync_stream = client.stream_from(latest_block + 1).await?;

        // Start streaming...
        tokio::spawn(async move {
            // Collect the specified number of latest events.
            while let Some(message) = rewind_stream.next().await {
                if !Self::try_broadcast(&rewind_tx, message) {
                    return;
                }
            }

            // Close the rewind channel sender to signal to the log consumers that the rewind phase
            // is done.
            drop(rewind_tx);

            // Since both rewind and live log consumers are ultimately streaming to the same
            // channel, we must ensure that all latest events are streamed before
            // consuming the live stream, otherwise the log consumers may send events out
            // of order.
            rewind_log_consumers.join_all().await;

            // Notify the client that we're now streaming live.
            if !Self::try_broadcast(&live_tx, ScannerStatus::SwitchingToLive) {
                return;
            }

            // Start the live (sync) stream.
            while let Some(message) = sync_stream.next().await {
                if !Self::try_broadcast(&live_tx, message) {
                    return;
                }
            }

            // This serves two purposes:
            // 1. Ensure that all log consumers finish streaming live events before the scanner is
            //    stopped
            // 2. If we didn't move the log consumer handles into this tokio task, they would've
            //    been dropped at the end of `scan_latest_then_live` scope, which would've stopped
            //    the live stream before it even begins.
            live_consumers.join_all().await;
        });

        Ok(())
    }

    fn spawn_log_consumers(
        &self,
        range_tx: &Sender<BlockRangeMessage>,
        mode: ConsumerMode,
    ) -> JoinSet<()> {
        self.event_listeners.iter().fold(JoinSet::new(), |mut set, listener| {
            let provider = self.block_range_scanner.provider().clone();
            let filter = listener.filter.clone();
            let base_filter = Filter::from(&filter);
            let sender = listener.sender.clone();
            let mut sub = range_tx.subscribe();

            set.spawn(async move {
                // Only used for CollectLatest
                let mut collected: Vec<Log> = match mode {
                    ConsumerMode::CollectLatest { count } => Vec::with_capacity(count),
                    ConsumerMode::Stream => Vec::new(),
                };

                loop {
                    let next = sub.recv().await;

                    match next {
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
                            info!(status = ?status, "Received status message");
                            if !Self::try_send(&sender, status).await {
                                break;
                            }
                        }
                        Err(RecvError::Closed) => {
                            warn!("No block ranges to receive, dropping receiver.");
                            break;
                        }
                        Err(RecvError::Lagged(_)) => {}
                    }
                }

                if let ConsumerMode::CollectLatest { .. } = mode {
                    println!("Sending collected logs: {collected:?}");
                    if !collected.is_empty() {
                        collected.reverse(); // restore chronological order
                    }

                    _ = Self::try_send(&sender, collected).await;
                }
            });

            set
        })
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

    #[must_use]
    fn try_broadcast<T: Into<BlockRangeMessage>>(
        sender: &broadcast::Sender<BlockRangeMessage>,
        message: T,
    ) -> bool {
        if let Err(err) = sender.send(message.into()) {
            warn!(error = %err, "No receivers, stopping broadcast");
            return false;
        }
        true
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
    /// Selects live, historical, or historical→live mode based on `start_height`/`end_height`.
    ///
    /// # Arguments
    ///
    /// * `start_height` - Start block (tag or number).
    /// * `end_height` - Optional end block (tag or number). If `Some`, a historical scan is
    ///   performed over the inclusive range; if `None`, the scanner either streams live or performs
    ///   historical→live depending on `start_height`.
    ///
    /// # Reorg behavior
    ///
    /// * Historical: No reorg detection still WIP.
    /// * Live: emits [`ScannerStatus::ReorgDetected`] and adjusts the confirmed range using
    ///   `with_block_confirmations` (re-emits confirmed portions as needed).
    /// * Historical → Live: reorgs are handled as per the particular mode the scanner is in
    ///   (historical or live).
    ///
    /// # Errors
    ///
    /// Returns an error if the scanner fails to start
    ///
    /// [`ScannerStatus::ReorgDetected`]: crate::types::ScannerStatus::ReorgDetected
    pub async fn start_scanner<T: Into<BlockNumberOrTag>>(
        self,
        start_height: T,
        end_height: Option<T>,
    ) -> Result<(), EventScannerError> {
        self.event_scanner.start(start_height.into(), end_height.map(Into::into)).await
    }

    /// Scans the chain and collects the latest `count` events per registered listener.
    ///
    /// Internally calls `scan_latest_in_range` with `Earliest..=Latest` and emits a single message
    /// per listener with up to `count` logs, chronologically ordered.
    ///
    /// # Reorg behavior
    ///
    /// Same as `scan_latest_in_range` over the full chain; reorgs during rewind are detected,
    /// [`ScannerStatus::ReorgDetected`] is emitted, and the reorg is handled by restarting from
    /// the updated tip.
    ///
    /// # Arguments
    ///
    /// * `count` - Maximum number of events to return per listener.
    ///
    /// # Errors
    ///
    /// Returns `EventScannerError` if the scan fails to start or fetching logs fails.
    ///
    /// [`ScannerStatus::ReorgDetected`]: crate::types::ScannerStatus::ReorgDetected
    pub async fn scan_latest(self, count: usize) -> Result<(), EventScannerError> {
        self.event_scanner
            .scan_latest(count, BlockNumberOrTag::Earliest, BlockNumberOrTag::Latest)
            .await
    }

    /// Scans within the provided block range and collects the latest `count` events per registered
    /// listener.
    ///
    /// Emits a single message per listener with up to `count` logs, chronologically ordered.
    ///
    /// # Arguments
    ///
    /// * `count` - Maximum number of events to return per listener.
    /// * `start_height` - Inclusive start block (tag or number).
    /// * `end_height` - Inclusive end block (tag or number).
    ///
    /// # Reorg behavior
    ///
    /// Reverse-ordered rewind over the range with periodic tip checks. On reorg, emits
    /// [`ScannerStatus::ReorgDetected`], resets the rewind start to the updated tip, and resumes.
    ///
    /// # Errors
    ///
    /// Returns `EventScannerError` if the scan fails to start or fetching logs fails.
    ///
    /// [`ScannerStatus::ReorgDetected`]: crate::types::ScannerStatus::ReorgDetected
    pub async fn scan_latest_in_range<T: Into<BlockNumberOrTag>>(
        self,
        count: usize,
        start_height: T,
        end_height: T,
    ) -> Result<(), EventScannerError> {
        self.event_scanner.scan_latest(count, start_height.into(), end_height.into()).await
    }

    pub async fn scan_latest_then_live(self, count: usize) -> Result<(), EventScannerError> {
        self.event_scanner.scan_latest_then_live(count.into()).await
    }
}
