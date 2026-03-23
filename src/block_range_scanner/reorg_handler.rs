use alloy::{
    consensus::BlockHeader,
    eips::{BlockNumHash, BlockNumberOrTag},
    network::{BlockResponse, Ethereum, Network, primitives::HeaderResponse},
    primitives::{B256, BlockNumber},
};
use robust_provider::RobustProvider;

use super::ring_buffer::RingBuffer;
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

/// Default implementation of [`ReorgHandler`] that uses parent hash verification
/// against a ring buffer of `(block_number, block_hash)` pairs.
///
/// # Core Invariant
///
/// Every incoming block's parent hash is verified against the buffer, rather than
/// checking on-chain hash existence. This is cleaner and more performant for common
/// reorg scenarios.
///
/// # Scenarios Handled
///
/// - **Happy path**: Next sequential block whose parent hash matches buffer back.
/// - **Scenario 1 (stream rewind)**: Block within buffer range, parent matches → truncate + push.
/// - **Scenario 2 (past fork)**: Block within buffer range, parent mismatch → walk back via RPC.
/// - **Scenario 3 (gap)**: Block beyond buffer head → fetch gap blocks, verify chain.
/// - **Scenario 4 (deep reorg)**: Block before buffer range → reset to finalized.
/// - **Duplicate**: Same number and hash as buffer back → no-op.
#[derive(Clone, Debug)]
pub(crate) struct DefaultReorgHandler<N: Network = Ethereum> {
    provider: RobustProvider<N>,
    buffer: RingBuffer,
}

impl<N: Network> ReorgHandler<N> for DefaultReorgHandler<N> {
    /// Checks an incoming block for reorgs using on-chain hash verification and
    /// parent hash validation against a ring buffer.
    ///
    /// Returns `Ok(None)` if no reorg, or `Ok(Some(common_ancestor))` if a reorg was detected.
    ///
    /// # Edge Cases
    ///
    /// * **Empty buffer** - First block is simply added; no reorg detection possible.
    /// * **Duplicate block** - Same number and hash as buffer back is a no-op.
    /// * **Gap in block numbers** - Intermediate blocks are fetched and verified for chain
    ///   continuity.
    /// * **Deep reorg beyond buffer capacity** - Falls back to the finalized block.
    /// * **Network errors** - Propagated immediately, not treated as reorgs.
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(level = "trace", fields(block.hash = %block.header().hash(), block.number = block.header().number()))
    )]
    async fn check(
        &mut self,
        block: &N::BlockResponse,
    ) -> Result<Option<N::BlockResponse>, ScannerError> {
        let header = block.header();
        let incoming = BlockNumHash::new(header.number(), header.hash());
        let parent_hash = header.parent_hash();

        // ── On-chain verification ────────────────────────────────────
        // Essential when called with a stored block (e.g., previous_batch_end).
        // If the hash no longer exists on the canonical chain, the block was reorged.
        if self.reorg_detected(incoming.hash).await? {
            return self.find_on_chain_ancestor().await;
        }

        // ── Case 0: Empty buffer ─────────────────────────────────────
        if self.buffer.is_empty() {
            trace!(block_number = incoming.number, "Buffer empty, adding first block");
            self.buffer.push(incoming);
            return Ok(None);
        }

        // SAFETY: buffer is non-empty after the check above
        let buffer_front = *self.buffer.front().unwrap();
        let buffer_back = *self.buffer.back().unwrap();

        // ── Duplicate block (no-op) ──────────────────────────────────
        if incoming.number == buffer_back.number && incoming.hash == buffer_back.hash {
            trace!(block_number = incoming.number, "Duplicate block, skipping");
            return Ok(None);
        }

        // ── Happy path: next sequential block ────────────────────────
        if incoming.number == buffer_back.number + 1 && parent_hash == buffer_back.hash {
            trace!(block_number = incoming.number, "Next sequential block, parent hash matches");
            self.buffer.push(incoming);
            return Ok(None);
        }

        // ── Scenario 1 & 2: Block within buffer range ───────────────
        if incoming.number >= buffer_front.number && incoming.number <= buffer_back.number {
            return self.handle_reorg_within_buffer(block, incoming, parent_hash).await;
        }

        // ── Scenario 3: Gap (block beyond buffer head) ──────────────
        if incoming.number > buffer_back.number + 1 {
            return self.handle_gap(block, incoming, parent_hash).await;
        }

        // ── Scenario 4: Block before buffer range (deep reorg) ──────
        if incoming.number < buffer_front.number {
            return self.handle_deep_reorg().await;
        }

        // Fallback: next block but parent hash mismatch (reorg at tip)
        // incoming.number == buffer_back.number + 1, but parent_hash != buffer_back.hash
        self.handle_reorg_within_buffer(block, incoming, parent_hash).await
    }
}

impl<N: Network> DefaultReorgHandler<N> {
    pub fn new(provider: RobustProvider<N>, capacity: RingBufferCapacity) -> Self {
        Self { provider, buffer: RingBuffer::new(capacity) }
    }

    /// Checks if a block hash still exists on the canonical chain via RPC.
    async fn reorg_detected(&self, hash: B256) -> Result<bool, ScannerError> {
        match self.provider.get_block_by_hash(hash).await {
            Ok(_) => Ok(false),
            Err(robust_provider::Error::BlockNotFound) => Ok(true),
            Err(e) => Err(e.into()),
        }
    }

    /// Walks backwards through the buffer, checking each hash on-chain to find
    /// the common ancestor after a reorg is detected.
    async fn find_on_chain_ancestor(&mut self) -> Result<Option<N::BlockResponse>, ScannerError> {
        debug!("Reorg detected via on-chain verification, searching for common ancestor");

        let (front_number, back_number) = match (self.buffer.front(), self.buffer.back()) {
            (Some(f), Some(b)) => (f.number, b.number),
            _ => return self.handle_deep_reorg().await,
        };

        for number in (front_number..=back_number).rev() {
            if let Some(entry) = self.buffer.get(number) {
                match self.provider.get_block_by_hash(entry.hash).await {
                    Ok(_) => {
                        debug!(
                            common_ancestor_number = number,
                            common_ancestor_hash = %entry.hash,
                            "Found common ancestor in buffer"
                        );
                        self.buffer.truncate_after(number);
                        return self.return_reorg_ancestor(number).await;
                    }
                    Err(robust_provider::Error::BlockNotFound) => {
                        trace!(
                            block_number = number,
                            block_hash = %entry.hash,
                            "Buffered block was reorged, continuing walk-back"
                        );
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        }

        // Entire buffer exhausted — all buffered blocks were reorged
        self.handle_deep_reorg().await
    }

    /// Handles blocks that fall within the buffer range — the stream rewound to a block
    /// we already have stored. Covers Scenario 1 (clean rewind) and Scenario 2 (past fork).
    async fn handle_reorg_within_buffer(
        &mut self,
        incoming_block: &N::BlockResponse,
        incoming: BlockNumHash,
        parent_hash: B256,
    ) -> Result<Option<N::BlockResponse>, ScannerError> {
        let fork_number = incoming.number.saturating_sub(1);

        // Check if parent is in our buffer and matches
        if let Some(stored) = self.buffer.get(fork_number) &&
            parent_hash == stored.hash
        {
            // ── Scenario 1: Clean rewind, fork point found ───────
            debug!(
                fork_point = fork_number,
                incoming_block = incoming.number,
                "Reorg detected (Scenario 1): stream rewound to fork point"
            );
            self.buffer.truncate_after(fork_number);
            self.buffer.push(incoming);

            return self.return_reorg_ancestor(fork_number).await;
        }

        // ── Scenario 2: Parent mismatch, walk back via RPC ──────────
        self.walk_back_to_find_fork(incoming_block, incoming).await
    }

    /// Walks back via RPC to find the fork point when the incoming block's parent
    /// doesn't match what's in our buffer (Scenario 2).
    async fn walk_back_to_find_fork(
        &mut self,
        _incoming_block: &N::BlockResponse,
        incoming: BlockNumHash,
    ) -> Result<Option<N::BlockResponse>, ScannerError> {
        let buffer_front = match self.buffer.front() {
            Some(f) => *f,
            None => return self.handle_deep_reorg().await,
        };

        let mut new_blocks = vec![incoming];
        let mut current_number = incoming.number;

        loop {
            let parent_number = current_number.saturating_sub(1);

            // If we've walked past our buffer, we can't verify further
            if parent_number < buffer_front.number {
                debug!(
                    parent_number = parent_number,
                    buffer_front = buffer_front.number,
                    "Walked past buffer range during fork search"
                );
                return self.handle_deep_reorg().await;
            }

            // Fetch the parent block from the node
            let parent_block = self.provider.get_block_by_number(parent_number.into()).await?;
            let parent_header = parent_block.header();
            let parent_entry = BlockNumHash::new(parent_header.number(), parent_header.hash());
            let grandparent_hash = parent_header.parent_hash();

            new_blocks.push(parent_entry);

            // Check if this block's parent matches our buffer
            let grandparent_number = parent_number.saturating_sub(1);
            if let Some(stored) = self.buffer.get(grandparent_number) &&
                grandparent_hash == stored.hash
            {
                // Found the fork point
                let fork_point = grandparent_number;
                debug!(
                    fork_point = fork_point,
                    walk_back_depth = incoming.number - parent_number,
                    "Reorg detected (Scenario 2): found fork point by walking back"
                );

                self.buffer.truncate_after(fork_point);
                // Push new blocks in order (they were collected newest-first)
                new_blocks.reverse();
                self.buffer.append(new_blocks);

                return self.return_reorg_ancestor(fork_point).await;
            }

            current_number = parent_number;
        }
    }

    /// Handles the case where the stream jumped ahead (gap between buffer back and incoming).
    /// Fetches missing blocks and verifies chain continuity (Scenario 3).
    async fn handle_gap(
        &mut self,
        incoming_block: &N::BlockResponse,
        incoming: BlockNumHash,
        parent_hash: B256,
    ) -> Result<Option<N::BlockResponse>, ScannerError> {
        let buffer_back = *self.buffer.back().unwrap();
        let gap_start = buffer_back.number + 1;
        let gap_end = incoming.number; // exclusive: we handle incoming separately

        debug!(
            buffer_tip = buffer_back.number,
            incoming = incoming.number,
            gap_size = gap_end - gap_start,
            "Gap detected, fetching intermediate blocks"
        );

        // Fetch all gap blocks and record their parent hashes for verification
        let mut gap_blocks = Vec::new();
        let mut gap_parent_hashes: Vec<B256> = Vec::new();
        for num in gap_start..gap_end {
            let block = self.provider.get_block_by_number(num.into()).await?;
            let h = block.header();
            gap_parent_hashes.push(h.parent_hash());
            gap_blocks.push(BlockNumHash::new(h.number(), h.hash()));
        }

        // Verify the first gap block connects to our buffer
        if let Some(first_gap_parent) = gap_parent_hashes.first() &&
            *first_gap_parent != buffer_back.hash
        {
            // A reorg happened that affected our buffer tail.
            // Treat the first gap block as a reorg block.
            debug!(
                expected_parent = %buffer_back.hash,
                actual_parent = %first_gap_parent,
                "Gap block doesn't connect to buffer, reorg in gap detected"
            );
            let first_entry = gap_blocks[0];
            return self
                .handle_reorg_within_buffer(incoming_block, first_entry, *first_gap_parent)
                .await;
        }

        // Verify incoming block connects to the gap chain
        let expected_parent = gap_blocks.last().map_or(buffer_back.hash, |b| b.hash);
        if parent_hash != expected_parent {
            debug!(
                expected = %expected_parent,
                actual = %parent_hash,
                "Incoming block doesn't connect to gap chain"
            );
            return self.walk_back_to_find_fork(incoming_block, incoming).await;
        }

        // Everything connects — push gap blocks and incoming into buffer
        self.buffer.append(gap_blocks);
        self.buffer.push(incoming);

        trace!(buffer_tip = incoming.number, "Gap filled successfully, no reorg");
        Ok(None)
    }

    /// Handles deep reorg where the incoming block is before the buffer range (Scenario 4).
    /// Falls back to the latest finalized block as a safe anchor.
    async fn handle_deep_reorg(&mut self) -> Result<Option<N::BlockResponse>, ScannerError> {
        info!("Deep reorg detected (block before buffer range), falling back to finalized block");

        let finalized = self.provider.get_block_by_number(BlockNumberOrTag::Finalized).await?;
        let finalized_header = finalized.header();

        debug!(
            finalized_number = finalized_header.number(),
            finalized_hash = %finalized_header.hash(),
            "Resetting buffer to finalized block"
        );

        self.buffer.clear();
        self.buffer.push(BlockNumHash::new(finalized_header.number(), finalized_header.hash()));

        Ok(Some(finalized))
    }

    /// Fetches the common ancestor block and returns it, after verifying it's not
    /// before the finalized block.
    async fn return_reorg_ancestor(
        &mut self,
        fork_block_number: BlockNumber,
    ) -> Result<Option<N::BlockResponse>, ScannerError> {
        let ancestor = self.provider.get_block_by_number(fork_block_number.into()).await?;
        let ancestor_number = ancestor.header().number();

        // Verify ancestor is not before finalized
        let finalized = self.provider.get_block_by_number(BlockNumberOrTag::Finalized).await?;
        let finalized_number = finalized.header().number();

        if ancestor_number >= finalized_number {
            debug!(
                common_ancestor_number = ancestor_number,
                common_ancestor_hash = %ancestor.header().hash(),
                "Returning common ancestor"
            );
            Ok(Some(ancestor))
        } else {
            warn!(
                ancestor_number = ancestor_number,
                finalized_number = finalized_number,
                "Common ancestor predates finalized block, falling back to finalized"
            );
            self.buffer.clear();
            self.buffer.push(BlockNumHash::new(finalized_number, finalized.header().hash()));
            Ok(Some(finalized))
        }
    }
}
