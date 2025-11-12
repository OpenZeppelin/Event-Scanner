use alloy::{
    consensus::BlockHeader,
    eips::BlockNumberOrTag,
    network::{BlockResponse, Network},
};

use tokio::sync::mpsc;
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tracing::info;

use crate::{
    EventScannerBuilder, ScannerError, ScannerStatus,
    block_range_scanner::Message as BlockRangeMessage,
    event_scanner::{
        EventScanner,
        scanner::{
            SyncFromLatestEvents,
            common::{ConsumerMode, handle_stream},
        },
    },
    robust_provider::IntoRobustProvider,
};

impl EventScannerBuilder<SyncFromLatestEvents> {
    #[must_use]
    pub fn block_confirmations(mut self, confirmations: u64) -> Self {
        self.config.block_confirmations = confirmations;
        self
    }

    /// Connects to an existing provider.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The provider connection fails
    /// - The event count is zero
    pub async fn connect<N: Network>(
        self,
        provider: impl IntoRobustProvider<N>,
    ) -> Result<EventScanner<SyncFromLatestEvents, N>, ScannerError> {
        if self.config.count == 0 {
            return Err(ScannerError::InvalidEventCount);
        }
        self.build(provider).await
    }
}

impl<N: Network> EventScanner<SyncFromLatestEvents, N> {
    /// Starts the scanner.
    ///
    /// # Important notes
    ///
    /// * Register event streams via [`scanner.subscribe(filter)`][subscribe] **before** calling
    ///   this function.
    /// * The method returns immediately; events are delivered asynchronously.
    ///
    /// # Errors
    ///
    /// Can error out if the service fails to start.
    ///
    /// [subscribe]: EventScanner::subscribe
    #[allow(clippy::missing_panics_doc)]
    pub async fn start(self) -> Result<(), ScannerError> {
        let count = self.config.count;
        let provider = self.block_range_scanner.provider().clone();
        let listeners = self.listeners.clone();

        info!(count = count, "Starting scanner, mode: fetch latest events and switch to live");

        let client = self.block_range_scanner.run()?;

        // Fetch the latest block number.
        // This is used to determine the starting point for the rewind stream and the live
        // stream. We do this before starting the streams to avoid a race condition
        // where the latest block changes while we're setting up the streams.
        let latest_block =
            provider.get_block_by_number(BlockNumberOrTag::Latest).await?.header().number();

        // Setup rewind and live streams to run in parallel.
        let rewind_stream = client.rewind(BlockNumberOrTag::Earliest, latest_block).await?;
        // We actually rely on the sync mode for the live stream, to
        // ensure that we don't miss any events in case a new block was minted while
        // we were setting up the streams or a reorg happens.
        let sync_stream =
            client.stream_from(latest_block + 1, self.config.block_confirmations).await?;

        // Start streaming...
        tokio::spawn(async move {
            // Since both rewind and live log consumers are ultimately streaming to the same
            // channel, we must ensure that all latest events are streamed before
            // consuming the live stream, otherwise the log consumers may send events out
            // of order.
            handle_stream(
                rewind_stream,
                &provider,
                &listeners,
                ConsumerMode::CollectLatest { count },
            )
            .await;

            // Notify the client that we're now streaming live.
            info!("Switching to live stream");

            // Use a one-off channel for the notification.
            let (tx, rx) = mpsc::channel::<BlockRangeMessage>(1);
            let stream = ReceiverStream::new(rx);
            tx.send(BlockRangeMessage::Status(ScannerStatus::SwitchingToLive))
                .await
                .expect("receiver exists");

            // close the channel to stop the stream
            drop(tx);

            let sync_stream = stream.chain(sync_stream);

            // Start the live (sync) stream.
            handle_stream(sync_stream, &provider, &listeners, ConsumerMode::Stream).await;
        });

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use alloy::{network::Ethereum, providers::{RootProvider, mock::Asserter}, rpc::client::RpcClient};

    use super::*;

    #[tokio::test]
    async fn test_sync_from_latest_returns_error_with_zero_count() {
        let provider = RootProvider::<Ethereum>::new(RpcClient::mocked(Asserter::new()));
        let result = EventScannerBuilder::sync().from_latest(0).connect(provider).await;
        
        match result {
            Err(ScannerError::InvalidEventCount) => {}
            _ => panic!("Expected InvalidEventCount error"),
        }
    }
}
