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

    pub async fn check(
        &mut self,
        incoming_block: N::HeaderResponse,
    ) -> Result<Option<BlockNumber>, ScannerError> {
        if !self.reorg_detected().await? {
            //
            self.buffer.push(incoming_block.hash());
            return Ok(None);
        }

        info!("Reorg detected, searching for common ancestor");

        // last block hash definitely doesn't exist on-chain
        _ = self.buffer.pop_back();

        while let Some(&block_hash) = self.buffer.back() {
            info!(block_hash = %block_hash, "Checking if block exists on-chain");
            match self.provider.get_block_by_hash(block_hash).await {
                Ok(block) => {
                    let header = block.header();
                    info!(common_ancestor = %header.hash(), block_number = header.number(), "Common ancestor found");
                    // store the incoming block's hash for future reference
                    self.buffer.push(incoming_block.hash());
                    return Ok(Some(header.number()));
                }
                Err(robust_provider::Error::BlockNotFound(_)) => {
                    _ = self.buffer.pop_back();
                }
                Err(e) => return Err(e.into()),
            }
        }

        warn!("Deep reorg detected, setting finalized block as common ancestor");

        let finalized = self.provider.get_block_by_number(BlockNumberOrTag::Finalized).await?;

        // no need to store finalized block's hash in the buffer, as it is returned by default only
        // if not buffered hashes exist on-chain

        // store the incoming block's hash for future reference
        self.buffer.push(incoming_block.hash());

        let header = finalized.header();
        info!(common_ancestor = %header.hash(), block_number = header.number(), "Finalized block set as common ancestor");

        Ok(Some(header.number()))
    }

    async fn reorg_detected(&self) -> Result<bool, ScannerError> {
        match self.buffer.back() {
            Some(last_streamed_block_hash) => {
                match self.provider.get_block_by_hash(*last_streamed_block_hash).await {
                    Ok(_) => Ok(false),
                    Err(robust_provider::Error::BlockNotFound(_)) => Ok(true),
                    Err(e) => Err(e.into()),
                }
            }
            None => Ok(false),
        }
    }
}
