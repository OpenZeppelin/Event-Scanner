use alloy::{
    consensus::BlockHeader,
    eips::BlockNumberOrTag,
    network::{BlockResponse, Ethereum, Network, primitives::HeaderResponse},
    primitives::{BlockHash, BlockNumber},
};
use tracing::{info, warn};

use crate::{
    ScannerError,
    robust_provider::{self, RobustProvider},
};

use super::ring_buffer::RingBuffer;

#[derive(Clone)]
pub(crate) struct ReorgHandler<N: Network = Ethereum> {
    provider: RobustProvider<N>,
    buffer: RingBuffer<BlockHash>,
}

impl<N: Network> ReorgHandler<N> {
    pub fn new(provider: RobustProvider<N>) -> Self {
        Self { provider, buffer: RingBuffer::new(10) }
    }

    pub async fn check_by_block_number(
        &mut self,
        block: impl Into<BlockNumberOrTag>,
    ) -> Result<Option<BlockNumber>, ScannerError> {
        let block = self.provider.get_block_by_number(block.into()).await?;
        self.check(block.header()).await
    }

    pub async fn check(
        &mut self,
        block: &N::HeaderResponse,
    ) -> Result<Option<BlockNumber>, ScannerError> {
        if !self.reorg_detected(block).await? {
            // store the incoming block's hash for future reference
            self.buffer.push(block.hash());
            return Ok(None);
        }

        info!("Reorg detected, searching for common ancestor");

        while let Some(&block_hash) = self.buffer.back() {
            info!(block_hash = %block_hash, "Checking if block exists on-chain");
            match self.provider.get_block_by_hash(block_hash).await {
                Ok(common_ancestor) => {
                    let common_ancestor = common_ancestor.header();

                    let finalized =
                        self.provider.get_block_by_number(BlockNumberOrTag::Finalized).await?;
                    let finalized = finalized.header();

                    let common_ancestor = if finalized.number() <= common_ancestor.number() {
                        info!(common_ancestor = %common_ancestor.hash(), block_number = common_ancestor.number(), "Common ancestor found");
                        common_ancestor
                    } else {
                        warn!(
                            finalized_hash = %finalized.hash(), block_number = finalized.number(), "Possible deep reorg detected, using finalized block as common ancestor"
                        );
                        // all buffered blocks are finalized, so no more need to track them
                        self.buffer.clear();
                        finalized
                    };

                    return Ok(Some(common_ancestor.number()));
                }
                Err(robust_provider::Error::BlockNotFound(_)) => {
                    // block was reorged
                    _ = self.buffer.pop_back();
                }
                Err(e) => return Err(e.into()),
            }
        }

        warn!("Possible deep reorg detected, setting finalized block as common ancestor");

        let finalized = self.provider.get_block_by_number(BlockNumberOrTag::Finalized).await?;

        // no need to store finalized block's hash in the buffer, as it is returned by default only
        // if not buffered hashes exist on-chain

        let header = finalized.header();
        info!(finalized_hash = %header.hash(), block_number = header.number(), "Finalized block set as common ancestor");

        Ok(Some(header.number()))
    }

    async fn reorg_detected(&self, block: &N::HeaderResponse) -> Result<bool, ScannerError> {
        match self.provider.get_block_by_hash(block.hash()).await {
            Ok(_) => Ok(false),
            Err(robust_provider::Error::BlockNotFound(_)) => Ok(true),
            Err(e) => Err(e.into()),
        }
    }
}
