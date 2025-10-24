use alloy::eips::BlockNumberOrTag;

use crate::event_scanner::modes::common::{ConsumerMode, handle_stream};

pub(crate) mod from_block;
pub(crate) mod from_latest;

use from_block::SyncFromBlockEventScannerBuilder;
use from_latest::SyncFromLatestScannerBuilder;

pub struct SyncScannerBuilder;

impl SyncScannerBuilder {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self
    }

    /// Scans the latest `count` matching events per registered listener, then automatically
    /// transitions to live streaming mode.
    ///
    /// This method combines two scanning phases into a single operation:
    /// 1. **Latest events phase**: Collects up to `count` most recent events by scanning backwards
    ///    from the current chain tip
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
    /// # Example
    ///
    /// ```no_run
    /// # use alloy::network::Ethereum;
    /// # use event_scanner::{EventFilter, EventScanner, EventScannerMessage};
    /// # use tokio_stream::StreamExt;
    /// #
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let ws_url = "ws://localhost:8545".parse()?;
    /// # let contract_address = alloy::primitives::address!("0xd8dA6BF26964af9d7eed9e03e53415d37aa96045");
    /// let mut scanner = EventScanner::sync()
    ///     .from_latest(10)
    ///     .connect_ws::<Ethereum>(ws_url)
    ///     .await?;
    ///
    /// let filter = EventFilter::new().contract_address(contract_address);
    /// let mut stream = scanner.subscribe(filter);
    ///
    /// scanner.start().await?;
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
    /// # Usage Notes
    ///
    /// - Call [`subscribe`](Self::subscribe) to register listeners **before** calling this method,
    ///   otherwise no events will be delivered.
    /// - The method returns immediately after spawning the scanning task. Events are delivered
    ///   asynchronously through the registered streams.
    /// - The live phase continues indefinitely until the scanner is dropped or an error occurs.
    ///
    /// [`ScannerStatus::ReorgDetected`]: crate::types::ScannerStatus::ReorgDetected
    /// [`ScannerStatus::SwitchingToLive`]: crate::types::ScannerStatus::SwitchingToLive
    #[must_use]
    pub fn from_latest(self, count: usize) -> SyncFromLatestScannerBuilder {
        SyncFromLatestScannerBuilder::new(count)
    }

    #[must_use]
    pub fn from_block(
        self,
        block: impl Into<BlockNumberOrTag>,
    ) -> SyncFromBlockEventScannerBuilder {
        SyncFromBlockEventScannerBuilder::new(block.into())
    }
}
