use alloy::{
    consensus::BlockHeader,
    eips::BlockNumberOrTag,
    network::{BlockResponse, Ethereum, Network, primitives::HeaderResponse},
    primitives::BlockHash,
};

use super::ring_buffer::RingBuffer;
use crate::{
    ScannerError,
    block_range_scanner::ring_buffer::RingBufferCapacity,
    robust_provider::{self, RobustProvider},
};

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
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(level = "trace", fields(block.hash = %block.header().hash(), block.number = block.header().number()))
    )]
    pub async fn check(
        &mut self,
        block: &N::BlockResponse,
    ) -> Result<Option<N::BlockResponse>, ScannerError> {
        let block = block.header();

        if !self.reorg_detected(block).await? {
            let block_hash = block.hash();
            // store the incoming block's hash for future reference
            if !matches!(self.buffer.back(), Some(&hash) if hash == block_hash) {
                self.buffer.push(block_hash);
                trace!(
                    block_number = block.number(),
                    block_hash = %block_hash,
                    "Block hash added to reorg buffer"
                );
            }
            return Ok(None);
        }

        debug!(
            block_number = block.number(),
            block_hash = %block.hash(),
            "Reorg detected, searching for common ancestor"
        );

        while let Some(&block_hash) = self.buffer.back() {
            trace!(block_hash = %block_hash, "Checking if buffered block exists on chain");
            match self.provider.get_block_by_hash(block_hash).await {
                Ok(common_ancestor) => {
                    debug!(
                        common_ancestor_hash = %block_hash,
                        common_ancestor_number = common_ancestor.header().number(),
                        "Found common ancestor"
                    );
                    return self.return_common_ancestor(common_ancestor).await;
                }
                Err(robust_provider::Error::BlockNotFound(_)) => {
                    // block was reorged
                    trace!(block_hash = %block_hash, "Buffered block was reorged, removing from buffer");
                    _ = self.buffer.pop_back();
                }
                Err(e) => return Err(e.into()),
            }
        }

        // return last finalized block as common ancestor

        // no need to store finalized block's hash in the buffer, as it is returned by default only
        // if not buffered hashes exist on-chain

        info!("No common ancestors found in buffer, falling back to finalized block");

        let finalized = self.provider.get_block_by_number(BlockNumberOrTag::Finalized).await?;

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
            debug!(
                common_ancestor_number = common_ancestor_header.number(),
                common_ancestor_hash = %common_ancestor_header.hash(),
                "Returning common ancestor"
            );
            common_ancestor
        } else {
            warn!(
                common_ancestor_number = common_ancestor_header.number(),
                common_ancestor_hash = %common_ancestor_header.hash(),
                finalized_number = finalized_header.number(),
                finalized_hash = %finalized_header.hash(),
                "Found common ancestor predates finalized block, falling back to finalized"
            );
            // all buffered blocks are finalized, so no more need to track them
            self.buffer.clear();
            finalized
        };
        Ok(Some(common_ancestor))
    }
}
