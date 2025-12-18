use alloy::{
    consensus::BlockHeader,
    eips::BlockNumberOrTag,
    network::{BlockResponse, Ethereum, Network, primitives::HeaderResponse},
    primitives::BlockHash,
};
use tracing::{info, warn};

use crate::{
    ScannerError,
    block_range_scanner::ring_buffer::RingBufferCapacity,
    robust_provider::{self, RobustProvider},
};

use super::ring_buffer::RingBuffer;

#[derive(Clone, Debug)]
pub(crate) struct ReorgHandler<N: Network = Ethereum> {
    provider: RobustProvider<N>,
    buffer: RingBuffer<BlockHash>,
}

impl<N: Network> ReorgHandler<N> {
    pub fn new(provider: RobustProvider<N>, capacity: RingBufferCapacity) -> Self {
        Self { provider, buffer: RingBuffer::new(capacity) }
    }

    /// Checks if a block was reorged and returns the common ancestor if found.
    ///
    /// # Arguments
    ///
    /// * `block` - The block to check for reorg.
    ///
    /// # Returns
    ///
    /// * `Ok(Some(common_ancestor))` - If a reorg was detected, returns the common ancestor block.
    /// * `Ok(None)` - If no reorg was detected, returns `None`.
    /// * `Err(e)` - If an error occurred while checking for reorg.
    ///
    /// # Edge Cases
    ///
    /// * **Duplicate block detection** - If the incoming block hash matches the last buffered hash,
    ///   it won't be added again to prevent buffer pollution from duplicate checks.
    ///
    /// * **Empty buffer on reorg** - If a reorg is detected but the buffer is empty (e.g., first
    ///   block after initialization), the function falls back to the finalized block as the common
    ///   ancestor.
    ///
    /// * **Deep reorg beyond buffer capacity** - If all buffered blocks are reorged (buffer
    ///   exhausted), the finalized block is used as a safe fallback to prevent data loss.
    ///
    /// * **Common ancestor beyond finalized** - This can happen if not all sequental blocks are
    ///   checked and stored. If the found common ancestor has a lower block number than the
    ///   finalized block, the finalized block is used instead and the buffer is cleared.
    ///
    /// * **Network errors during lookup** - Non-`BlockNotFound` errors (e.g., RPC failures) are
    ///   propagated immediately rather than being treated as reorgs.
    ///
    /// * **Finalized block unavailable** - If the finalized block cannot be fetched when needed as
    ///   a fallback, the error is propagated to the caller.
    pub async fn check(
        &mut self,
        block: &N::BlockResponse,
    ) -> Result<Option<N::BlockResponse>, ScannerError> {
        let block = block.header();
        info!(block_hash = %block.hash(), block_number = block.number(), "Checking if block was reorged");

        if !self.reorg_detected(block).await? {
            let block_hash = block.hash();
            info!(block_hash = %block_hash, block_number = block.number(), "No reorg detected");
            // store the incoming block's hash for future reference
            if !matches!(self.buffer.back(), Some(&hash) if hash == block_hash) {
                self.buffer.push(block_hash);
            }
            return Ok(None);
        }

        info!("Reorg detected, searching for common ancestor");

        while let Some(&block_hash) = self.buffer.back() {
            info!(block_hash = %block_hash, "Checking if block exists on-chain");
            match self.provider.get_block_by_hash(block_hash).await {
                Ok(common_ancestor) => return self.return_common_ancestor(common_ancestor).await,
                Err(robust_provider::Error::BlockNotFound(_)) => {
                    // block was reorged
                    _ = self.buffer.pop_back();
                }
                Err(e) => return Err(e.into()),
            }
        }

        // return last finalized block as common ancestor

        // no need to store finalized block's hash in the buffer, as it is returned by default only
        // if not buffered hashes exist on-chain

        warn!("Possible deep reorg detected, setting finalized block as common ancestor");

        let finalized = self.provider.get_block_by_number(BlockNumberOrTag::Finalized).await?;

        let header = finalized.header();
        info!(finalized_hash = %header.hash(), block_number = header.number(), "Finalized block set as common ancestor");

        Ok(Some(finalized))
    }

    async fn reorg_detected(&self, block: &N::HeaderResponse) -> Result<bool, ScannerError> {
        match self.provider.get_block_by_hash(block.hash()).await {
            Ok(_) => Ok(false),
            Err(robust_provider::Error::BlockNotFound(_)) => Ok(true),
            Err(e) => Err(e.into()),
        }
    }

    async fn return_common_ancestor(
        &mut self,
        common_ancestor: <N as Network>::BlockResponse,
    ) -> Result<Option<N::BlockResponse>, ScannerError> {
        let common_ancestor_header = common_ancestor.header();
        let finalized = self.provider.get_block_by_number(BlockNumberOrTag::Finalized).await?;
        let finalized_header = finalized.header();
        let common_ancestor = if finalized_header.number() <= common_ancestor_header.number() {
            info!(common_ancestor = %common_ancestor_header.hash(), block_number = common_ancestor_header.number(), "Common ancestor found");
            common_ancestor
        } else {
            warn!(
                finalized_hash = %finalized_header.hash(), block_number = finalized_header.number(), "Possible deep reorg detected, using finalized block as common ancestor"
            );
            // all buffered blocks are finalized, so no more need to track them
            self.buffer.clear();
            finalized
        };
        Ok(Some(common_ancestor))
    }
}
