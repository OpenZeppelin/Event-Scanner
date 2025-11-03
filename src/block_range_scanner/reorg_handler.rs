use alloy::{
    network::{Ethereum, Network},
    primitives::{BlockHash, BlockNumber},
};

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
            return Ok(None);
        }

        // warn!(reorged_from = reorged_from, "Reorg detected: sending forked range");

        Ok(Some(1))
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
