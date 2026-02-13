use alloy::{
    consensus::BlockHeader,
    eips::BlockNumberOrTag,
    network::{BlockResponse, Ethereum, Network, primitives::HeaderResponse},
    primitives::{BlockHash, BlockNumber},
};
use robust_provider::RobustProvider;

use super::ring_buffer::{BlockInfo, RingBuffer};
use crate::{ScannerError, block_range_scanner::ring_buffer::RingBufferCapacity};

/// Trait for handling chain reorganizations.
#[allow(async_fn_in_trait)]
pub trait ReorgHandler<N: Network> {
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
    async fn check(
        &mut self,
        block: &N::BlockResponse,
    ) -> Result<Option<N::BlockResponse>, ScannerError>;
}

/// Default implementation of [`ReorgHandler`] that uses an RPC provider.
#[derive(Clone, Debug)]
pub(crate) struct DefaultReorgHandler<N: Network = Ethereum> {
    provider: RobustProvider<N>,
    buffer: RingBuffer<BlockInfo<BlockHash>>,
}

impl<N: Network> ReorgHandler<N> for DefaultReorgHandler<N> {
    /// Checks if a block was reorged and returns the common ancestor if found.
    ///
    /// This implementation uses a hybrid approach:
    /// - For previously seen blocks (duplicates), verifies they still exist on-chain
    /// - For new blocks, verifies parent hash against the buffer for efficient detection
    /// - Falls back to finalized block for deep reorgs beyond buffer capacity
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
    /// * **Duplicate block detection** - If the incoming block matches a buffered block, it's
    ///   verified on-chain to detect reorgs affecting previously processed blocks.
    ///
    /// * **Empty buffer** - If the buffer is empty, no reorg detection is possible. The block is
    ///   simply added to the buffer.
    ///
    /// * **Gap in block numbers** - If there's a gap between the incoming block and the buffer,
    ///   intermediate blocks are fetched to verify chain continuity.
    ///
    /// * **Deep reorg beyond buffer capacity** - If all buffered blocks are reorged (buffer
    ///   exhausted), the finalized block is used as a safe fallback.
    ///
    /// * **Common ancestor beyond finalized** - If the found common ancestor has a lower block
    ///   number than the finalized block, the finalized block is used instead.
    ///
    /// * **Network errors during lookup** - Non-`BlockNotFound` errors (e.g., RPC failures) are
    ///   propagated immediately rather than being treated as reorgs.
    async fn check(
        &mut self,
        block: &N::BlockResponse,
    ) -> Result<Option<N::BlockResponse>, ScannerError> {
        let header = block.header();
        let block_number = header.number();
        let block_hash = header.hash();
        let parent_hash = header.parent_hash();

        debug!(
            block_hash = %block_hash,
            block_number = block_number,
            parent_hash = %parent_hash,
            "Checking block for reorg"
        );

        // Handle empty buffer case - no reorg detection possible yet
        if self.buffer.is_empty() {
            info!(block_number = block_number, "Buffer empty, adding first block");
            self.buffer.push(BlockInfo { number: block_number, hash: block_hash });
            return Ok(None);
        }

        // Check if this is a previously seen block (duplicate check)
        // This happens when checking `previous_batch_end` to verify it wasn't reorged
        if let Some(buffered) = self.buffer.find_by_number(block_number) {
            if buffered.hash == block_hash {
                // This is a known block - verify it still exists on-chain
                debug!(
                    block_number = block_number,
                    "Block already in buffer, verifying on-chain existence"
                );
                return self.verify_block_on_chain(block).await;
            }
            // Same number but different hash - definite reorg
            info!(
                block_number = block_number,
                buffered_hash = %buffered.hash,
                incoming_hash = %block_hash,
                "Block number matches but hash differs, reorg detected"
            );
            return self.find_common_ancestor(block).await;
        }

        // New block - use parent hash verification
        let parent_number = block_number.saturating_sub(1);

        // Try to find the parent in our buffer
        if let Some(buffered_parent) = self.buffer.find_by_number(parent_number) {
            if buffered_parent.hash == parent_hash {
                // Parent hash matches - no reorg, add block to buffer
                debug!(
                    block_number = block_number,
                    parent_number = parent_number,
                    "Parent hash matches buffer, no reorg detected"
                );
                self.buffer.push(BlockInfo { number: block_number, hash: block_hash });
                return Ok(None);
            }

            // Parent hash mismatch - reorg detected!
            info!(
                block_number = block_number,
                parent_number = parent_number,
                expected_hash = %buffered_parent.hash,
                actual_hash = %parent_hash,
                "Parent hash mismatch, reorg detected"
            );
            return self.find_common_ancestor(block).await;
        }

        // Parent not in buffer - might be a gap, need to verify chain continuity
        self.handle_gap(block, parent_number).await
    }
}

impl<N: Network> DefaultReorgHandler<N> {
    pub fn new(provider: RobustProvider<N>, capacity: RingBufferCapacity) -> Self {
        Self { provider, buffer: RingBuffer::new(capacity) }
    }

    /// Verifies that a previously seen block still exists on-chain.
    /// Used for duplicate blocks to detect if they were reorged out.
    async fn verify_block_on_chain(
        &mut self,
        block: &N::BlockResponse,
    ) -> Result<Option<N::BlockResponse>, ScannerError> {
        let header = block.header();
        let block_hash = header.hash();

        match self.provider.get_block_by_hash(block_hash).await {
            Ok(_) => {
                // Block still exists on-chain, no reorg
                debug!(block_number = header.number(), "Block verified on-chain, no reorg");
                Ok(None)
            }
            Err(robust_provider::Error::BlockNotFound) => {
                // Block was reorged out
                info!(
                    block_number = header.number(),
                    block_hash = %block_hash,
                    "Block no longer exists on-chain, reorg detected"
                );
                self.find_common_ancestor(block).await
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Handles the case where there's a gap between the incoming block and the buffer.
    /// Fetches intermediate blocks to verify chain continuity.
    async fn handle_gap(
        &mut self,
        incoming_block: &N::BlockResponse,
        parent_number: BlockNumber,
    ) -> Result<Option<N::BlockResponse>, ScannerError> {
        let incoming_header = incoming_block.header();
        let block_number = incoming_header.number();
        let block_hash = incoming_header.hash();

        // Get the latest block we have in buffer
        let Some(last_buffered) = self.buffer.back().copied() else {
            // Buffer became empty somehow, just add the block
            self.buffer.push(BlockInfo { number: block_number, hash: block_hash });
            return Ok(None);
        };

        // If incoming block is not ahead of buffer, something is wrong
        if block_number <= last_buffered.number {
            // Block is at or before our buffer tip - this could indicate a reorg
            // where we're receiving an alternative chain
            info!(
                incoming_number = block_number,
                buffer_tip = last_buffered.number,
                "Incoming block not ahead of buffer, checking for reorg"
            );
            return self.find_common_ancestor(incoming_block).await;
        }

        debug!(
            buffer_tip = last_buffered.number,
            incoming_number = block_number,
            gap_size = block_number - last_buffered.number - 1,
            "Gap detected, fetching intermediate blocks"
        );

        // Fetch blocks from (last_buffered.number + 1) to parent_number to verify chain
        let mut current_number = last_buffered.number + 1;
        let mut expected_parent_hash = last_buffered.hash;

        while current_number <= parent_number {
            let intermediate_block =
                match self.provider.get_block_by_number(current_number.into()).await {
                    Ok(block) => block,
                    Err(e) => {
                        warn!(
                            block_number = current_number,
                            error = %e,
                            "Failed to fetch intermediate block"
                        );
                        return Err(e.into());
                    }
                };

            let intermediate_header = intermediate_block.header();
            let intermediate_hash = intermediate_header.hash();
            let intermediate_parent = intermediate_header.parent_hash();

            // Verify this block's parent matches what we expect
            if intermediate_parent != expected_parent_hash {
                info!(
                    block_number = current_number,
                    expected_parent = %expected_parent_hash,
                    actual_parent = %intermediate_parent,
                    "Chain discontinuity detected during gap fill, reorg detected"
                );
                return self.find_common_ancestor(incoming_block).await;
            }

            // Add to buffer and continue
            self.buffer.push(BlockInfo { number: current_number, hash: intermediate_hash });
            expected_parent_hash = intermediate_hash;
            current_number += 1;
        }

        // Now verify the incoming block's parent hash
        if incoming_header.parent_hash() != expected_parent_hash {
            info!(
                block_number = block_number,
                expected_parent = %expected_parent_hash,
                actual_parent = %incoming_header.parent_hash(),
                "Incoming block parent mismatch after gap fill, reorg detected"
            );
            return self.find_common_ancestor(incoming_block).await;
        }

        // Chain is continuous, add the incoming block
        self.buffer.push(BlockInfo { number: block_number, hash: block_hash });
        Ok(None)
    }

    /// Finds the common ancestor by walking back through the buffer and verifying
    /// each block's existence on-chain.
    async fn find_common_ancestor(
        &mut self,
        _incoming_block: &N::BlockResponse,
    ) -> Result<Option<N::BlockResponse>, ScannerError> {
        info!("Searching for common ancestor");

        // Walk back through buffer to find a block that still exists on-chain
        while let Some(block_info) = self.buffer.pop_back() {
            debug!(
                block_number = block_info.number,
                block_hash = %block_info.hash,
                "Checking if buffered block exists on-chain"
            );

            match self.provider.get_block_by_hash(block_info.hash).await {
                Ok(block) => {
                    // Found a block that exists on-chain - this is our common ancestor
                    return self.validate_and_return_ancestor(block).await;
                }
                Err(robust_provider::Error::BlockNotFound) => {
                    // Block was reorged, continue walking back
                    debug!(
                        block_number = block_info.number,
                        "Block was reorged, continuing search"
                    );
                }
                Err(e) => {
                    // Network error, propagate it
                    return Err(e.into());
                }
            }
        }

        // Buffer exhausted - fall back to finalized block
        self.fallback_to_finalized().await
    }

    /// Validates the common ancestor against the finalized block and returns it.
    async fn validate_and_return_ancestor(
        &mut self,
        common_ancestor: N::BlockResponse,
    ) -> Result<Option<N::BlockResponse>, ScannerError> {
        let ancestor_header = common_ancestor.header();
        let ancestor_number = ancestor_header.number();

        let finalized = self.provider.get_block_by_number(BlockNumberOrTag::Finalized).await?;
        let finalized_header = finalized.header();
        let finalized_number = finalized_header.number();

        if ancestor_number >= finalized_number {
            info!(
                common_ancestor_number = ancestor_number,
                common_ancestor_hash = %ancestor_header.hash(),
                "Common ancestor found"
            );
            // Truncate buffer to remove reorged blocks
            self.buffer.truncate_from(ancestor_number + 1);
            Ok(Some(common_ancestor))
        } else {
            warn!(
                ancestor_number = ancestor_number,
                finalized_number = finalized_number,
                "Common ancestor is before finalized block, using finalized as ancestor"
            );
            self.buffer.clear();
            Ok(Some(finalized))
        }
    }

    /// Falls back to the finalized block when buffer is exhausted.
    async fn fallback_to_finalized(&mut self) -> Result<Option<N::BlockResponse>, ScannerError> {
        warn!("Buffer exhausted, falling back to finalized block as common ancestor");

        let finalized = self.provider.get_block_by_number(BlockNumberOrTag::Finalized).await?;

        info!(
            finalized_number = finalized.header().number(),
            finalized_hash = %finalized.header().hash(),
            "Using finalized block as common ancestor"
        );

        self.buffer.clear();
        Ok(Some(finalized))
    }
}
