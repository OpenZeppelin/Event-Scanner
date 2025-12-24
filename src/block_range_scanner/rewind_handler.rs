use std::cmp::Ordering;

use alloy::{
    consensus::BlockHeader,
    eips::{BlockId, BlockNumberOrTag},
    network::{BlockResponse, Network},
};
use tokio::{sync::mpsc, try_join};

use crate::{
    Notification, ScannerError,
    block_range_scanner::{
        common::BlockScannerResult, range_iterator::RangeIterator, reorg_handler::ReorgHandler,
        ring_buffer::RingBufferCapacity,
    },
    robust_provider::RobustProvider,
    types::TryStream,
};

pub(crate) struct RewindHandler<N: Network> {
    provider: RobustProvider<N>,
    max_block_range: u64,
    start_id: BlockId,
    end_id: BlockId,
    sender: mpsc::Sender<BlockScannerResult>,
    reorg_handler: ReorgHandler<N>,
}

impl<N: Network> RewindHandler<N> {
    pub fn new(
        provider: RobustProvider<N>,
        max_block_range: u64,
        start_id: BlockId,
        end_id: BlockId,
        past_blocks_storage_capacity: RingBufferCapacity,
        sender: mpsc::Sender<BlockScannerResult>,
    ) -> Self {
        let reorg_handler = ReorgHandler::new(provider.clone(), past_blocks_storage_capacity);
        Self { provider, max_block_range, start_id, end_id, sender, reorg_handler }
    }

    pub async fn run(self) -> Result<(), ScannerError> {
        let RewindHandler {
            provider,
            max_block_range,
            start_id,
            end_id,
            sender,
            mut reorg_handler,
        } = self;

        let (start_block, end_block) =
            try_join!(provider.get_block(start_id), provider.get_block(end_id))?;

        // normalize block range: from (higher) -> to (lower)
        let (from, to) = match start_block.header().number().cmp(&end_block.header().number()) {
            Ordering::Greater => (start_block, end_block),
            _ => (end_block, start_block),
        };

        let from_num = from.header().number();
        let to_num = to.header().number();
        info!(
            from_block = from_num,
            to_block = to_num,
            total_blocks = from_num.saturating_sub(to_num) + 1,
            "Starting rewind stream"
        );

        tokio::spawn(async move {
            Self::handle_stream_rewind(
                from,
                to,
                max_block_range,
                &sender,
                &provider,
                &mut reorg_handler,
            )
            .await;
            debug!("Rewind stream ended");
        });

        Ok(())
    }

    /// Streams blocks in reverse order from `from` to `to`.
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(level = "trace", skip(sender, provider, reorg_handler))
    )]
    async fn handle_stream_rewind(
        from: N::BlockResponse,
        to: N::BlockResponse,
        max_block_range: u64,
        sender: &mpsc::Sender<BlockScannerResult>,
        provider: &RobustProvider<N>,
        reorg_handler: &mut ReorgHandler<N>,
    ) {
        // for checking whether reorg occurred
        let mut tip = from;

        let from = tip.header().number();
        let to = to.header().number();

        let finalized_block = match provider.get_block_by_number(BlockNumberOrTag::Finalized).await
        {
            Ok(block) => block,
            Err(e) => {
                error!("Failed to get finalized block for rewind");
                _ = sender.try_stream(e).await;
                return;
            }
        };

        let finalized_number = finalized_block.header().number();

        // only check reorg if our tip is after the finalized block
        let check_reorg = tip.header().number() > finalized_number;
        debug!(
            from = from,
            to = to,
            finalized = finalized_number,
            check_reorg = check_reorg,
            "Rewind stream configuration"
        );

        let mut iter = RangeIterator::reverse(from, to, max_block_range);
        for range in &mut iter {
            // stream the range regularly, i.e. from smaller block number to greater
            if !sender.try_stream(range).await {
                break;
            }

            if check_reorg {
                let reorg = match reorg_handler.check(&tip).await {
                    Ok(opt) => opt,
                    Err(e) => {
                        error!("Failed to perform reorg check");
                        _ = sender.try_stream(e).await;
                        return;
                    }
                };

                if let Some(common_ancestor) = reorg {
                    info!(
                        common_ancestor = common_ancestor.header().number(),
                        tip = tip.header().number(),
                        "Reorg detected during rewind, rescanning affected blocks"
                    );
                    if !Self::handle_reorg_rescan(
                        &mut tip,
                        common_ancestor,
                        max_block_range,
                        sender,
                        provider,
                    )
                    .await
                    {
                        return;
                    }
                }
            }
        }
    }

    /// Handles re-scanning of reorged blocks.
    ///
    /// Returns `true` on success, `false` if stream closed or terminal error occurred.
    async fn handle_reorg_rescan(
        tip: &mut N::BlockResponse,
        common_ancestor: N::BlockResponse,
        max_block_range: u64,
        sender: &mpsc::Sender<BlockScannerResult>,
        provider: &RobustProvider<N>,
    ) -> bool {
        let tip_number = tip.header().number();
        let common_ancestor = common_ancestor.header().number();

        debug!(
            tip_number = tip_number,
            common_ancestor = common_ancestor,
            blocks_to_rescan = tip_number.saturating_sub(common_ancestor),
            "Rescanning reorged blocks"
        );

        if !sender.try_stream(Notification::ReorgDetected { common_ancestor }).await {
            return false;
        }

        // Get the new tip block (same height as original tip, but new hash)
        *tip = match provider.get_block_by_number(tip_number.into()).await {
            Ok(block) => {
                trace!(
                    new_tip_number = block.header().number(),
                    "Fetched new tip block after reorg"
                );
                block
            }
            Err(e) => {
                if matches!(e, crate::robust_provider::Error::BlockNotFound(_)) {
                    error!(
                        tip_number = tip_number,
                        "Unexpected: chain height decreased after reorg"
                    );
                }
                _ = sender.try_stream(e).await;
                return false;
            }
        };

        // Re-scan only the affected range (from common_ancestor + 1 up to tip)
        let rescan_from = common_ancestor + 1;

        for batch in RangeIterator::forward(rescan_from, tip_number, max_block_range) {
            trace!(range_start = *batch.start(), range_end = *batch.end(), "Rescanning batch");
            if !sender.try_stream(batch).await {
                return false;
            }
        }

        true
    }
}
