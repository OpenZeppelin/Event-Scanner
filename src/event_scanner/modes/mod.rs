mod common;
mod historic;
mod latest;
mod live;
mod sync;

pub use historic::{HistoricEventScanner, HistoricScannerBuilder};
pub use latest::{LatestEventScanner, LatestScannerBuilder};
pub use live::{LiveEventScanner, LiveScannerBuilder};
pub use sync::{
    SyncScannerBuilder,
    from_block::{SyncFromBlockEventScanner, SyncFromBlockEventScannerBuilder},
    from_latest::{SyncFromLatestEventScanner, SyncFromLatestScannerBuilder},
};

pub struct EventScanner;

impl EventScanner {
    #[must_use]
    pub fn historic() -> HistoricScannerBuilder {
        HistoricScannerBuilder::new()
    }

    #[must_use]
    pub fn live() -> LiveScannerBuilder {
        LiveScannerBuilder::new()
    }

    #[must_use]
    pub fn sync() -> SyncScannerBuilder {
        SyncScannerBuilder::new()
    }

    /// Streams the latest `count` matching events per registered listener.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use alloy::{network::Ethereum, primitives::Address};
    /// # use event_scanner::{EventFilter, EventScanner, Message};
    /// # use tokio_stream::StreamExt;
    /// #
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let ws_url = "ws://localhost:8545".parse()?;
    /// # let contract_address = alloy::primitives::address!("0xd8dA6BF26964af9d7eed9e03e53415d37aa96045");
    /// // Collect the latest 10 events across Earliest..=Latest
    /// let mut scanner = EventScanner::latest()
    ///     .count(10)
    ///     .connect_ws::<Ethereum>(ws_url)
    ///     .await?;
    ///
    /// let filter = EventFilter::new().contract_address(contract_address);
    /// let mut stream = scanner.subscribe(filter);
    ///
    /// scanner.start().await?;
    ///
    /// // Expect a single message with up to 10 logs, then the stream ends
    /// while let Some(Message::Data(logs)) = stream.next().await {
    ///     println!("Latest logs: {}", logs.len());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Restricting to a specific block range:
    ///
    /// ```no_run
    /// # use alloy::network::Ethereum;
    /// # use event_scanner::EventScanner;
    /// #
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let ws_url = "ws://localhost:8545".parse()?;
    /// // Collect the latest 5 events between blocks [1_000_000, 1_100_000]
    /// let mut scanner = EventScanner::latest()
    ///     .count(5)
    ///     .from_block(1_000_000)
    ///     .to_block(1_100_000)
    ///     .connect_ws::<Ethereum>(ws_url)
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # How it works
    ///
    /// The scanner performs a reverse-ordered scan (newest to oldest) within the specified block
    /// range, collecting up to `count` events per registered listener. Once the target count is
    /// reached or the range is exhausted, it delivers the events in chronological order (oldest to
    /// newest) and completes.
    ///
    /// When using a custom block range, the scanner automatically normalizes the range boundaries.
    /// This means you can specify `from_block` and `to_block` in any order - the scanner will
    /// always scan from the higher block number down to the lower one, regardless of which
    /// parameter holds which value.
    ///
    /// # Key behaviors
    ///
    /// - **Single delivery**: Each registered stream receives at most `count` logs in a single
    ///   message, chronologically ordered
    /// - **One-shot operation**: The scanner completes after delivering messages; it does not
    ///   continue streaming
    /// - **Flexible count**: If fewer than `count` events exist in the range, returns all available
    ///   events
    /// - **Default range**: By default, scans from `Earliest` to `Latest` block
    /// - **Reorg handling**: Periodically checks the tip to detect reorgs during the scan
    ///
    /// # Important notes
    ///
    /// - Register event streams via [`scanner.subscribe(filter)`][subscribe] **before** calling
    ///   [`scanner.start()`][start]
    /// - The [`scanner.start()`][start] method returns immediately; events are delivered
    ///   asynchronously
    /// - For continuous streaming after collecting latest events, use
    ///   [`EventScanner::sync().from_latest(count)`][sync_from_latest] instead
    ///
    /// # Reorg behavior
    ///
    /// During the scan, the scanner periodically checks the tip to detect reorgs. On reorg
    /// detection:
    /// 1. Emits [`ScannerStatus::ReorgDetected`][reorg] to all listeners
    /// 2. Resets to the updated tip
    /// 3. Restarts the scan from the new tip
    /// 4. Continues until `count` events are collected
    ///
    /// Final delivery to log listeners preserves chronological order regardless of reorgs.
    ///
    /// [count]: latest::LatestScannerBuilder::count
    /// [from_block]: latest::LatestScannerBuilder::from_block
    /// [to_block]: latest::LatestScannerBuilder::to_block
    /// [block_confirmations]: latest::LatestScannerBuilder::block_confirmations
    /// [max_block_range]: latest::LatestScannerBuilder::max_block_range
    /// [subscribe]: latest::LatestEventScanner::subscribe
    /// [start]: latest::LatestEventScanner::start
    /// [sync_from_latest]: SyncScannerBuilder::from_latest
    /// [reorg]: crate::types::ScannerStatus::ReorgDetected
    #[must_use]
    pub fn latest() -> LatestScannerBuilder {
        LatestScannerBuilder::new()
    }
}
