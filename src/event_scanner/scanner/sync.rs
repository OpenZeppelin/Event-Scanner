use alloy::eips::BlockNumberOrTag;

pub(crate) mod from_block;
pub(crate) mod from_latest;

use crate::{
    EventScannerBuilder,
    event_scanner::scanner::{SyncFromBlock, SyncFromLatestEvents, Synchronize},
};

impl EventScannerBuilder<Synchronize> {
    /// Scans the latest `count` matching events per registered listener, then automatically
    /// transitions to live streaming mode.
    ///
    /// This method combines two scanning phases into a single operation:
    ///
    /// 1. **Latest events phase**: Collects up to `count` most recent events by scanning backwards
    ///    from the current chain tip
    /// 2. **Automatic transition**: Emits [`ScannerStatus::SwitchingToLive`][switch_to_live] to
    ///    signal the mode change
    /// 3. **Live streaming phase**: Continuously monitors and streams new events as they arrive
    ///    on-chain
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use alloy::network::Ethereum;
    /// # use event_scanner::{EventFilter, EventScanner, Message};
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
    ///         Message::Data(logs) => {
    ///             println!("Received {} events", logs.len());
    ///         }
    ///         Message::Status(status) => {
    ///             println!("Status update: {:?}", status);
    ///             // You'll see ScannerStatus::SwitchingToLive when transitioning
    ///         }
    ///         Message::Error(e) => {
    ///             eprintln!("Error: {}", e);
    ///         }
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # How it works
    ///
    /// The scanner captures the latest block number before starting to establish a clear boundary
    /// between phases. The historical phase scans from genesis block to the current latest block,
    /// while the live phase starts from the block after the latest block. This design prevents
    /// duplicate events and handles race conditions where new blocks arrive during setup.
    ///
    /// # Key behaviors
    ///
    /// - **No duplicates**: Events are not delivered twice across the phase transition
    /// - **Flexible count**: If fewer than `count` events exist, returns all available events
    /// - **Reorg handling**: Both phases handle reorgs appropriately:
    ///   - Historical phase: resets and rescans on reorg detection
    ///   - Live phase: resets stream to the first post-reorg block that satisfies the configured
    ///     block confirmations
    /// - **Continuous operation**: Live phase continues indefinitely until the scanner is dropped
    ///
    /// # Arguments
    ///
    /// * `count` - Maximum number of recent events to collect per listener before switching to live
    ///   streaming
    ///
    /// # Important notes
    ///
    /// - The live phase continues indefinitely until the scanner is dropped or encounters an error
    ///
    /// # Detailed reorg behavior
    ///
    /// - **Historical rewind phase**: Restart the scanner. On detecting a reorg, emits
    ///   [`ScannerStatus::ReorgDetected`][reorg], resets the rewind start to the new tip, and
    ///   continues until collectors accumulate `count` logs. Final delivery to listeners preserves
    ///   chronological order.
    /// - **Live streaming phase**: Starts from `latest_block + 1` and respects the configured block
    ///   confirmations. On reorg, emits [`ScannerStatus::ReorgDetected`][reorg], adjusts the next
    ///   confirmed window (possibly re-emitting confirmed portions), and continues streaming.
    ///
    /// [subscribe]: from_latest::SyncFromLatestEventScanner::subscribe
    /// [start]: from_latest::SyncFromLatestEventScanner::start
    /// [reorg]: crate::types::ScannerStatus::ReorgDetected
    /// [switch_to_live]: crate::types::ScannerStatus::SwitchingToLive
    #[must_use]
    pub fn from_latest(self, count: usize) -> EventScannerBuilder<SyncFromLatestEvents> {
        EventScannerBuilder::<SyncFromLatestEvents>::new(count)
    }

    /// Streams events from a specific starting block to the present, then automatically
    /// transitions to live streaming mode.
    ///
    /// This method combines two scanning phases into a single operation:
    ///
    /// 1. **Historical sync phase**: Streams events from `from_block` up to the current confirmed
    ///    tip
    /// 2. **Automatic transition**: Emits [`ScannerStatus::SwitchingToLive`][switch_to_live] to
    ///    signal the mode change
    /// 3. **Live streaming phase**: Continuously monitors and streams new events as they arrive
    ///    on-chain
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use alloy::network::Ethereum;
    /// # use event_scanner::{EventFilter, EventScannerBuilder, Message};
    /// # use tokio_stream::StreamExt;
    /// #
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let ws_url = "ws://localhost:8545".parse()?;
    /// # let contract_address = alloy::primitives::address!("0xd8dA6BF26964af9d7eed9e03e53415d37aa96045");
    /// // Sync from block 1_000_000 to present, then stream new events
    /// let mut scanner = EventScannerBuilder::sync()
    ///     .from_block(1_000_000)
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
    ///         Message::Data(logs) => {
    ///             println!("Received {} events", logs.len());
    ///         }
    ///         Message::Status(status) => {
    ///             println!("Status update: {:?}", status);
    ///             // You'll see ScannerStatus::SwitchingToLive when transitioning
    ///         }
    ///         Message::Error(e) => {
    ///             eprintln!("Error: {}", e);
    ///         }
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Using block tags:
    ///
    /// ```no_run
    /// # use alloy::{network::Ethereum, eips::BlockNumberOrTag};
    /// # use event_scanner::EventScannerBuilder;
    /// #
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let ws_url = "ws://localhost:8545".parse()?;
    /// // Sync from genesis block
    /// let mut scanner = EventScannerBuilder::sync()
    ///     .from_block(BlockNumberOrTag::Earliest)
    ///     .connect_ws::<Ethereum>(ws_url)
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # How it works
    ///
    /// The scanner first streams all events from the specified starting block up to the current
    /// confirmed tip (respecting `block_confirmations`). Once caught up, it seamlessly transitions
    /// to live mode and continues streaming new events as blocks are produced.
    ///
    /// # Key behaviors
    ///
    /// - **No duplicates**: Events are not delivered twice across the phase transition
    /// - **Chronological order**: Historical events are delivered oldest to newest
    /// - **Seamless transition**: Automatically switches to live mode when caught up
    /// - **Continuous operation**: Live phase continues indefinitely until the scanner is dropped
    ///
    /// # Arguments
    ///
    /// * `block` - Starting block number or tag (e.g., `Earliest`, `Latest`, or a specific number)
    ///
    /// # Important notes
    ///
    /// - The live phase continues indefinitely until the scanner is dropped or encounters an error
    ///
    /// # Reorg behavior
    ///
    /// - **Historical sync phase**: Streams events in chronological order without reorg detection
    /// - **Live streaming phase**: Respects the configured block confirmations. On reorg, emits
    ///   [`ScannerStatus::ReorgDetected`][reorg], adjusts the next confirmed window (possibly
    ///   re-emitting confirmed portions), and continues streaming.
    ///
    /// [subscribe]: from_latest::SyncFromLatestEventScanner::subscribe
    /// [start]: from_latest::SyncFromLatestEventScanner::start
    /// [reorg]: crate::types::ScannerStatus::ReorgDetected
    /// [switch_to_live]: crate::types::ScannerStatus::SwitchingToLive
    #[must_use]
    pub fn from_block(
        self,
        block: impl Into<BlockNumberOrTag>,
    ) -> EventScannerBuilder<SyncFromBlock> {
        EventScannerBuilder::<SyncFromBlock>::new(block.into())
    }
}
