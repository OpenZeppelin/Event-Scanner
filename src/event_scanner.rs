#![allow(unused)]

use std::{future, time::Duration};

use crate::{
    block_scanner::{BlockScanner, BlockScannerBuilder, OnBlocksFunc},
    types::{CallbackConfig, EventFilter},
};
use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    providers::{IpcConnect, Provider, RootProvider, WsConnect},
    pubsub::PubSubConnect,
    rpc::client::RpcClient,
    transports::TransportError,
};

pub struct EventScannerBuilder<N: Network> {
    block_scanner: BlockScannerBuilder<N>,
    tracked_events: Vec<EventFilter>,
    callback_config: CallbackConfig,
}

impl<N: Network> Default for EventScannerBuilder<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N: Network> EventScannerBuilder<N> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            block_scanner: BlockScannerBuilder::new(),
            tracked_events: Vec::new(),
            callback_config: CallbackConfig::default(),
        }
    }

    #[must_use]
    pub fn with_event_filter(mut self, filter: EventFilter) -> Self {
        self.tracked_events.push(filter);
        self
    }

    #[must_use]
    pub fn with_event_filters(mut self, filters: Vec<EventFilter>) -> Self {
        self.tracked_events.extend(filters);
        self
    }

    #[must_use]
    pub fn with_callback_config(mut self, cfg: CallbackConfig) -> Self {
        self.callback_config = cfg;
        self
    }

    #[must_use]
    pub fn with_blocks_read_per_epoch(&mut self, blocks_read_per_epoch: usize) -> &mut Self {
        self.block_scanner.with_blocks_read_per_epoch(blocks_read_per_epoch);
        self
    }

    #[must_use]
    pub fn with_start_height(&mut self, start_height: BlockNumberOrTag) -> &mut Self {
        self.block_scanner.with_start_height(start_height);
        self
    }

    #[must_use]
    pub fn with_end_height(&mut self, end_height: BlockNumberOrTag) -> &mut Self {
        self.block_scanner.with_end_height(end_height);
        self
    }

    #[must_use]
    pub fn with_on_blocks(&mut self, on_blocks: OnBlocksFunc<N>) -> &mut Self {
        self.block_scanner.with_on_blocks(on_blocks);
        self
    }

    #[must_use]
    pub fn with_reorg_rewind_depth(&mut self, reorg_rewind_depth: u64) -> &mut Self {
        self.block_scanner.with_reorg_rewind_depth(reorg_rewind_depth);
        self
    }

    #[must_use]
    pub fn with_retry_interval(&mut self, retry_interval: Duration) -> &mut Self {
        self.block_scanner.with_retry_interval(retry_interval);
        self
    }

    #[must_use]
    pub fn with_block_confirmations(&mut self, block_confirmations: u64) -> &mut Self {
        self.block_scanner.with_block_confirmations(block_confirmations);
        self
    }

    /// Connects to the provider via WebSocket
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ws(
        self,
        connect: WsConnect,
    ) -> Result<EventScanner<RootProvider<N>, N>, TransportError> {
        let block_scanner = self.block_scanner.connect_ws(connect).await?;
        Ok(EventScanner {
            block_scanner,
            tracked_events: self.tracked_events,
            callback_config: self.callback_config,
        })
    }

    /// Connects to the provider via IPC
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ipc<T>(
        self,
        connect: IpcConnect<T>,
    ) -> Result<EventScanner<RootProvider<N>, N>, TransportError>
    where
        IpcConnect<T>: PubSubConnect,
    {
        let block_scanner = self.block_scanner.connect_ipc(connect).await?;
        Ok(EventScanner {
            block_scanner,
            tracked_events: self.tracked_events,
            callback_config: self.callback_config,
        })
    }

    #[must_use]
    pub fn connect_client(self, client: RpcClient) -> EventScanner<RootProvider<N>, N> {
        let block_scanner = self.block_scanner.connect_client(client);
        EventScanner {
            block_scanner,
            tracked_events: self.tracked_events,
            callback_config: self.callback_config,
        }
    }

    #[must_use]
    pub fn connect_provider(self, provider: RootProvider<N>) -> EventScanner<RootProvider<N>, N> {
        let block_scanner = self.block_scanner.connect_provider(provider);
        EventScanner {
            block_scanner,
            tracked_events: self.tracked_events,
            callback_config: self.callback_config,
        }
    }
}

pub struct EventScanner<P: Provider<N>, N: Network> {
    block_scanner: BlockScanner<P, N>,
    tracked_events: Vec<EventFilter>,
    callback_config: CallbackConfig,
}

impl<P: Provider<N>, N: Network> EventScanner<P, N> {
    /// Starts the scanner
    ///
    /// # Errors
    ///
    /// Returns an error if the scanner fails to start
    pub async fn start(&mut self) -> anyhow::Result<()> {
        future::ready(()).await;
        todo!()
    }
}
