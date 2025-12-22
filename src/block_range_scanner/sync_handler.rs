use alloy::{eips::BlockId, network::Network, primitives::BlockNumber};
use tokio::sync::mpsc;

use crate::{
    Notification, ScannerError,
    block_range_scanner::{
        common::{self, BlockScannerResult},
        reorg_handler::ReorgHandler,
        ring_buffer::RingBufferCapacity,
    },
    robust_provider::RobustProvider,
    types::TryStream,
};

/// Represents the initial state when starting a sync operation
enum SyncState {
    /// Start block is already at or beyond the confirmed tip - go straight to live
    AlreadyLive { start_block: BlockNumber },
    /// Start block is behind - need to catch up first, then go live
    NeedsCatchup { start_block: BlockNumber, confirmed_tip: BlockNumber },
}

pub(crate) struct SyncHandler<N: Network> {
    provider: RobustProvider<N>,
    max_block_range: u64,
    start_id: BlockId,
    block_confirmations: u64,
    sender: mpsc::Sender<BlockScannerResult>,
    reorg_handler: ReorgHandler<N>,
}

impl<N: Network> SyncHandler<N> {
    pub fn new(
        provider: RobustProvider<N>,
        max_block_range: u64,
        start_id: BlockId,
        block_confirmations: u64,
        past_blocks_storage_capacity: RingBufferCapacity,
        sender: mpsc::Sender<BlockScannerResult>,
    ) -> Self {
        let reorg_handler = ReorgHandler::new(provider.clone(), past_blocks_storage_capacity);
        Self { provider, max_block_range, start_id, block_confirmations, sender, reorg_handler }
    }

    pub async fn run(mut self) -> Result<(), ScannerError> {
        let sync_state = self.determine_sync_state().await?;

        match sync_state {
            SyncState::AlreadyLive { start_block } => {
                info!(
                    start_block = start_block,
                    "Start block is beyond confirmed tip, waiting until starting block is confirmed before starting live stream"
                );
                self.spawn_live_only(start_block).await?;
            }
            SyncState::NeedsCatchup { start_block, confirmed_tip } => {
                info!(
                    start_block = start_block,
                    confirmed_tip = confirmed_tip,
                    "Start block is behind confirmed tip, catching up then transitioning to live"
                );
                self.spawn_catchup_then_live(start_block, confirmed_tip);
            }
        }

        Ok(())
    }

    /// Determines whether we need to catch up or can start live immediately
    async fn determine_sync_state(&self) -> Result<SyncState, ScannerError> {
        let (start_block, confirmed_tip) = tokio::try_join!(
            self.provider.get_block_number_by_id(self.start_id),
            self.provider.get_latest_confirmed(self.block_confirmations)
        )?;

        if start_block > confirmed_tip {
            Ok(SyncState::AlreadyLive { start_block })
        } else {
            Ok(SyncState::NeedsCatchup { start_block, confirmed_tip })
        }
    }

    /// Spawns a task that only streams live blocks (no historical catchup needed)
    async fn spawn_live_only(&mut self, start_block: BlockNumber) -> Result<(), ScannerError> {
        let max_block_range = self.max_block_range;
        let block_confirmations = self.block_confirmations;
        let provider = self.provider.clone();
        let sender = self.sender.clone();
        let mut reorg_handler = self.reorg_handler.clone();

        let subscription = provider.subscribe_blocks().await?;

        tokio::spawn(async move {
            common::stream_live_blocks(
                start_block,
                subscription,
                &sender,
                &provider,
                block_confirmations,
                max_block_range,
                &mut reorg_handler,
                true,
            )
            .await;
        });

        Ok(())
    }

    /// Spawns a task that catches up on historical blocks, then transitions to live streaming
    fn spawn_catchup_then_live(&self, start_block: BlockNumber, confirmed_tip: BlockNumber) {
        let max_block_range = self.max_block_range;
        let block_confirmations = self.block_confirmations;
        let provider = self.provider.clone();
        let mut reorg_handler = self.reorg_handler.clone();
        let sender = self.sender.clone();

        tokio::spawn(async move {
            // Phase 1: Catch up on any blocks that have been minted during the historical sync
            let start_block = match Self::catchup_historical_blocks(
                start_block,
                confirmed_tip,
                block_confirmations,
                max_block_range,
                &sender,
                &provider,
                &mut reorg_handler,
            )
            .await
            {
                Ok(Some(start_block)) => start_block,
                Ok(None) => {
                    return; // channel closed
                }
                Err(e) => {
                    error!(error = %e, "Error during historical catchup, shutting down");
                    _ = sender.try_stream(e).await;
                    return;
                }
            };

            // Phase 2: Transition to live streaming
            Self::transition_to_live(
                start_block,
                block_confirmations,
                max_block_range,
                &sender,
                &provider,
                &mut reorg_handler,
            )
            .await;
        });
    }

    /// Catches up on historical blocks until we reach the chain tip
    /// Returns the block number where live streaming should begin
    async fn catchup_historical_blocks(
        mut start_block: BlockNumber,
        mut confirmed_tip: BlockNumber,
        block_confirmations: u64,
        max_block_range: u64,
        sender: &mpsc::Sender<BlockScannerResult>,
        provider: &RobustProvider<N>,
        reorg_handler: &mut ReorgHandler<N>,
    ) -> Result<Option<BlockNumber>, ScannerError> {
        while start_block < confirmed_tip {
            if common::stream_historical_range(
                start_block,
                confirmed_tip,
                max_block_range,
                sender,
                provider,
                reorg_handler,
            )
            .await
            .is_none()
            {
                return Ok(None);
            }

            let latest = provider.get_block_number().await?;

            start_block = confirmed_tip + 1;
            confirmed_tip = latest.saturating_sub(block_confirmations);
        }

        info!("Historical catchup complete, ready to transition to live");

        Ok(Some(start_block))
    }

    /// Subscribes to live blocks and begins streaming
    async fn transition_to_live(
        start_block: BlockNumber,
        block_confirmations: u64,
        max_block_range: u64,
        sender: &mpsc::Sender<BlockScannerResult>,
        provider: &RobustProvider<N>,
        reorg_handler: &mut ReorgHandler<N>,
    ) {
        let subscription = match provider.subscribe_blocks().await {
            Ok(sub) => sub,
            Err(e) => {
                error!(error = %e, "Error subscribing to live blocks, shutting down");
                _ = sender.try_stream(e).await;
                return;
            }
        };

        if !sender.try_stream(Notification::SwitchingToLive).await {
            return;
        }

        info!("Successfully transitioned from historical to live streaming");

        common::stream_live_blocks(
            start_block,
            subscription,
            sender,
            provider,
            block_confirmations,
            max_block_range,
            reorg_handler,
            false, // (already notified above)
        )
        .await;
    }
}
