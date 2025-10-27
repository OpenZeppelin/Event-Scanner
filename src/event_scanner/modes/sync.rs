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
    /// 2. **Automatic transition**: Emits [`ScannerStatus::SwitchingToLive`] to signal the mode
    ///    change
    /// 3. **Live streaming phase**: Continuously monitors and streams new events as they arrive
    ///    on-chain
    ///
    /// # How it works
    ///
    /// The scanner captures the latest block number before starting to establish a clear boundary
    /// between phases. The historical phase scans from `Earliest` to `latest_block`, while the
    /// live phase starts from `latest_block + 1`. This design prevents duplicate events and
    /// handles race conditions where new blocks arrive during setup.
    ///
    /// # Key behaviors
    ///
    /// - **No duplicates**: Events are not delivered twice across the phase transition
    /// - **Flexible count**: If fewer than `count` events exist, returns all available events
    /// - **Reorg handling**: Both phases handle reorgs appropriately:
    ///   - Historical phase: resets and rescans on reorg detection
    ///   - Live phase: resets stream to the first post-reorg block that satisfies the block
    ///     confirmations set via [`block_confirmations`](Self::block_confirmations)
    /// - **Continuous operation**: Live phase continues indefinitely until the scanner is dropped
    ///
    /// # Arguments
    ///
    /// * `count` - Maximum number of recent events to collect per listener before switching to live
    ///   streaming
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
    /// // Fetch the latest 10 events, then stream new events continuously
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
    ///             println!("Status update: {:?}", status);
    ///             // You'll see ScannerStatus::SwitchingToLive when transitioning
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
    /// # Important notes
    ///
    /// - Register event streams via [`subscribe`](Self::subscribe) **before** calling
    ///   [`start`](Self::start)
    /// - The [`start`](Self::start) method returns immediately; events are delivered asynchronously
    /// - The live phase continues indefinitely until the scanner is dropped or encounters an error
    ///
    /// # Detailed reorg behavior
    ///
    /// - **Historical rewind phase**: Reverse-ordered rewind over `Earliest..=latest_block`. On
    ///   detecting a reorg, emits [`ScannerStatus::ReorgDetected`], resets the rewind start to the
    ///   new tip, and continues until collectors accumulate `count` logs. Final delivery to
    ///   listeners preserves chronological order.
    /// - **Live streaming phase**: Starts from `latest_block + 1` and respects block confirmations
    ///   configured via [`block_confirmations`](Self::block_confirmations). On reorg, emits
    ///   [`ScannerStatus::ReorgDetected`], adjusts the next confirmed window (possibly re-emitting
    ///   confirmed portions), and continues streaming.
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
