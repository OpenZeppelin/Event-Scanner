use alloy::eips::{BlockId, BlockNumberOrTag};

use crate::{DEFAULT_BLOCK_CONFIRMATIONS, DEFAULT_MAX_CONCURRENT_FETCHES};

/// Marker indicating that a scanner mode has not been selected yet.
#[derive(Default, Debug)]
pub struct Unspecified;

/// Mode marker for historical range scanning.
///
/// For more details on this scanner mode, see [`EventScannerBuilder::historic`].
#[derive(Debug)]
pub struct Historic {
    pub(crate) from_block: BlockId,
    pub(crate) to_block: BlockId,
    /// Controls how many log-fetching RPC requests can run in parallel during the scan.
    pub(crate) max_concurrent_fetches: usize,
}

/// Mode marker for live streaming.
///
/// For more details on this scanner mode, see [`EventScannerBuilder::live`].
#[derive(Debug)]
pub struct Live {
    pub(crate) block_confirmations: u64,
    /// Controls how many log-fetching RPC requests can run in parallel during the scan.
    pub(crate) max_concurrent_fetches: usize,
}

/// Mode marker for latest-events collection.
///
/// For more details on this scanner mode, see [`EventScannerBuilder::latest`].
#[derive(Debug)]
pub struct LatestEvents {
    pub(crate) count: usize,
    pub(crate) from_block: BlockId,
    pub(crate) to_block: BlockId,
    pub(crate) block_confirmations: u64,
    /// Controls how many log-fetching RPC requests can run in parallel during the scan.
    pub(crate) max_concurrent_fetches: usize,
}

/// Marker indicating that a sync mode must be selected.
#[derive(Default, Debug)]
pub struct Synchronize;

/// Mode marker for scanning by syncing from the specified count of latest events and then switching
/// to live mode.
///
/// For more details on this scanner mode, see
/// [`EventScannerBuilder::sync().from_latest(count)`](crate::EventScannerBuilder::from_latest).
#[derive(Debug)]
pub struct SyncFromLatestEvents {
    pub(crate) count: usize,
    pub(crate) block_confirmations: u64,
    /// Controls how many log-fetching RPC requests can run in parallel during the scan.
    pub(crate) max_concurrent_fetches: usize,
}

/// Mode marker for scanning by syncing from the specified block and then switching to live mode.
///
/// For more details on this scanner mode, see
/// [`EventScannerBuilder::sync().from_block(block_id)`][sync from block].
///
/// [sync from block]: crate::EventScannerBuilder#method.from_block-2
#[derive(Debug)]
pub struct SyncFromBlock {
    pub(crate) from_block: BlockId,
    pub(crate) block_confirmations: u64,
    /// Controls how many log-fetching RPC requests can run in parallel during the scan.
    pub(crate) max_concurrent_fetches: usize,
}

impl Default for Historic {
    fn default() -> Self {
        Self {
            from_block: BlockNumberOrTag::Earliest.into(),
            to_block: BlockNumberOrTag::Latest.into(),
            max_concurrent_fetches: DEFAULT_MAX_CONCURRENT_FETCHES,
        }
    }
}

impl Default for Live {
    fn default() -> Self {
        Self {
            block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS,
            max_concurrent_fetches: DEFAULT_MAX_CONCURRENT_FETCHES,
        }
    }
}
