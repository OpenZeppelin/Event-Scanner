#![allow(unused)]

use std::{future, marker::PhantomData, ops::Range, time::Duration};

use tokio::sync::{
    mpsc::{self, Receiver, Sender, error::SendError},
    oneshot,
};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};

use alloy::{
    consensus::BlockHeader,
    eips::{BlockId, BlockNumberOrTag, RpcBlockHash},
    network::{BlockResponse, Network, primitives::HeaderResponse},
    primitives::{BlockHash, BlockNumber},
    providers::{Provider, RootProvider},
    pubsub::{PubSubConnect, SubResultStream},
    rpc::{
        client::{ClientBuilder, RpcClient},
        types::Header,
    },
    transports::{
        RpcError, TransportError, TransportErrorKind, http::reqwest, ipc::IpcConnect, ws::WsConnect,
    },
};
use thiserror::Error;

use tracing::{debug, error, info, warn};

// copied form https://github.com/taikoxyz/taiko-mono/blob/f4b3a0e830e42e2fee54829326389709dd422098/packages/taiko-client/pkg/chain_iterator/block_batch_iterator.go#L19
const DEFAULT_BLOCKS_READ_PER_EPOCH: usize = 1000;
const DEFAULT_RETRY_INTERVAL: Duration = Duration::from_secs(12);
const DEFAULT_BLOCK_CONFIRMATIONS: u64 = 0;
const BACK_OFF_MAX_RETRIES: u64 = 5;

const CHANNEL_BUFFER_SIZE: usize = 10000;
const MAX_BUFFERED_MESSAGES: usize = 50000;

// TODO: determine check exact default value
const DEFAULT_REORG_REWIND_DEPTH: u64 = 0;

// State sync aware retry settings
const STATE_SYNC_RETRY_INTERVAL: Duration = Duration::from_secs(30);
const STATE_SYNC_MAX_RETRIES: u64 = 12;

#[derive(Error, Debug)]
pub enum SubscriptionError {
    #[error("HTTP request failed: {0}")]
    HttpError(#[from] reqwest::Error),

    // #[error("WebSocket error: {0}")]
    // WebSocketError(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("RPC error: {0}")]
    RpcError(#[from] alloy::transports::RpcError<alloy::transports::TransportErrorKind>),

    #[error("Channel send error")]
    ChannelError,

    #[error("Service is shutting down")]
    ServiceShutdown,

    #[error("Only one subscriber allowed at a time")]
    MultipleSubscribers,

    #[error("Historical sync failed: {0}")]
    HistoricalSyncError(String),

    #[error("WebSocket connection failed after {0} attempts")]
    WebSocketConnectionFailed(usize),
}

#[derive(Debug)]
pub enum Command<P: Provider<N>, N: Network> {
    Subscribe {
        sender: mpsc::Sender<Result<Range<BlockNumber>, SubscriptionError>>,
        provider: P,
        blocks_read_per_epoch: usize,
        start_height: BlockNumberOrTag,
        end_height: Option<BlockNumberOrTag>,
        reorg_rewind_depth: u64,
        retry_interval: Duration,
        block_confirmations: u64,
        response: oneshot::Sender<Result<(), SubscriptionError>>,
        network: PhantomData<fn() -> N>,
    },
    Unsubscribe {
        response: oneshot::Sender<Result<(), SubscriptionError>>,
    },
    Shutdown {
        response: oneshot::Sender<()>,
    },
}

struct Config {
    blocks_read_per_epoch: usize,
    start_height: BlockNumber,
    end_height: Option<BlockNumber>,
    reorg_rewind_depth: u64,
    retry_interval: Duration,
    block_confirmations: u64,
}

pub struct SubscriptionService<P: Provider<N>, N: Network> {
    config: Config,
    subscriber: Option<mpsc::Sender<Result<Range<BlockNumber>, SubscriptionError>>>,
    last_synced_block: Option<BlockHashAndNumber>,
    websocket_connected: bool,
    processed_count: u64,
    error_count: u64,
    command_receiver: mpsc::Receiver<Command<P, N>>,
    shutdown: bool,
}

impl<P: 'static + Provider<N> + Clone, N: Network> SubscriptionService<P, N> {
    pub fn new(config: Config) -> (Self, mpsc::Sender<Command<P, N>>) {
        let (cmd_tx, cmd_rx) = mpsc::channel(100);

        let service = Self {
            config,
            subscriber: None,
            last_synced_block: None,
            websocket_connected: false,
            processed_count: 0,
            error_count: 0,
            command_receiver: cmd_rx,
            shutdown: false,
        };

        (service, cmd_tx)
    }

    pub async fn run(mut self) {
        info!("Starting subscription service");

        while !self.shutdown {
            tokio::select! {
                cmd = self.command_receiver.recv() => {
                    match cmd {
                        Some(command) => {
                            if let Err(e) = self.handle_command(command).await {
                                error!("Command handling error: {}", e);
                                self.error_count += 1;
                            }
                        }
                        None => {
                            info!("Command channel closed, shutting down");
                            break;
                        }
                    }
                }
            }
        }

        info!("Subscription service stopped");
    }

    async fn handle_command(&mut self, command: Command<P, N>) -> Result<(), SubscriptionError> {
        match command {
            Command::Subscribe {
                sender,
                provider,
                blocks_read_per_epoch,
                start_height,
                end_height,
                reorg_rewind_depth,
                retry_interval,
                block_confirmations,
                response,
                network: _,
            } => {
                let result = self
                    .handle_subscribe(
                        sender,
                        provider,
                        blocks_read_per_epoch,
                        start_height,
                        end_height,
                        reorg_rewind_depth,
                        retry_interval,
                        block_confirmations,
                    )
                    .await;
                let _ = response.send(result);
            }
            Command::Unsubscribe { response } => {
                let result = self.handle_unsubscribe().await;
                let _ = response.send(result);
            }
            Command::Shutdown { response } => {
                self.shutdown = true;
                self.handle_unsubscribe().await?;
                let _ = response.send(());
            }
        }
        Ok(())
    }

    async fn handle_subscribe(
        &mut self,
        sender: mpsc::Sender<Result<Range<BlockNumber>, SubscriptionError>>,
        provider: P,
        blocks_read_per_epoch: usize,
        start_height: BlockNumberOrTag,
        end_height: Option<BlockNumberOrTag>,
        reorg_rewind_depth: u64,
        retry_interval: Duration,
        block_confirmations: u64,
    ) -> Result<(), SubscriptionError> {
        if self.subscriber.is_some() {
            return Err(SubscriptionError::MultipleSubscribers);
        }

        info!("Starting subscription from point: {:?}", start_height);
        self.subscriber = Some(sender);

        self.sync_with_transition(provider, start_height, end_height).await?;

        Ok(())
    }

    async fn sync_with_transition(
        &mut self,
        provider: P,
        start_height: BlockNumberOrTag,
        end_height: Option<BlockNumberOrTag>,
    ) -> Result<(), SubscriptionError> {
        // Step 1: Establish WebSocket connection and start buffering
        let (buffer_tx, buffer_rx) = mpsc::channel(MAX_BUFFERED_MESSAGES);

        let provider_clone = provider.clone();
        let ws_task =
            tokio::spawn(
                async move { Self::websocket_buffer_task(provider_clone, buffer_tx).await },
            );

        // Step 2: Perform historical sync
        let (start_block, sync_end_block) = match (start_height, end_height) {
            (_, Some(end_height)) => {
                let start_block =
                    provider.get_block_by_number(start_height).await?.expect("already checked");
                let end_height = provider
                    .get_block(end_height.into())
                    .await?
                    .expect("TODO: check if really valid");
                (start_block.header().number(), end_height)
            }
            (_, None) => {
                let start_block =
                    provider.get_block_by_number(start_height).await?.expect("already checked");
                let end_height = provider
                    .get_block(BlockId::Number(BlockNumberOrTag::Latest))
                    .await?
                    .expect("TODO: check if really valid");
                (start_block.header().number(), end_height)
            }
        };

        info!(
            "Syncing historical data from {} to {}",
            start_block,
            sync_end_block.header().number()
        );

        // TODO: invoke with smart retry mechanism with backoff
        if let Err(e) =
            self.sync_historical_data(start_block, sync_end_block.header().number()).await
        {
            ws_task.abort();
            return Err(SubscriptionError::HistoricalSyncError(e.to_string()));
        }

        // Step 3: Process buffered WebSocket messages
        let cutoff = sync_end_block.header().number();
        tokio::spawn(async move {
            self.process_buffered_messages(buffer_rx, cutoff).await;
        });

        // Step 4: Establish live WebSocket connection
        self.last_synced_block =
            Some(BlockHashAndNumber::from_header::<N>(sync_end_block.header()));

        info!("Successfully transitioned from historical to live data");
        Ok(())
    }

    async fn sync_historical_data(
        &mut self,
        from: BlockNumber,
        to: BlockNumber,
    ) -> Result<(), SubscriptionError> {
        let mut cursor = from;
        let mut batch_count = 0;

        while cursor < to {
            let batch_to = if cursor + self.config.blocks_read_per_epoch as u64 > to {
                to
            } else {
                cursor + self.config.blocks_read_per_epoch as u64
            };
            self.send_to_subscriber(Ok(cursor..batch_to)).await;
            cursor = batch_to;

            batch_count += 1;
            if batch_count % 10 == 0 {
                debug!("Processed {} historical batches", batch_count);
            }
        }

        info!("Historical sync completed: {} batches processed", batch_count);
        Ok(())
    }

    async fn websocket_buffer_task(provider: P, buffer_sender: mpsc::Sender<Range<BlockNumber>>) {
        let mut retry_count = 0;

        loop {
            match Self::connect_websocket(provider.clone()).await {
                Ok(mut ws_stream) => {
                    info!("WebSocket connected for buffering");
                    retry_count = 0;

                    while let Some(value) = ws_stream.next().await {
                        match value {
                            Ok(header_resp) => {
                                if buffer_sender
                                    .send((header_resp.number()..header_resp.number() + 1))
                                    .await
                                    .is_err()
                                {
                                    debug!("Buffer channel closed, stopping buffer task");
                                    return;
                                }
                            }
                            Err(e) => {
                                // TODO: handle error, e.g. stream closing
                                error!("WebSocket error during buffering: {}", e);
                                break;
                            }
                            _ => {}
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to connect WebSocket for buffering: {}", e);
                    retry_count += 1;
                    if retry_count >= 3 {
                        error!("Max retries reached for WebSocket buffering");
                        return;
                    }
                }
            }
        }
    }

    async fn process_buffered_messages(
        &mut self,
        mut buffer_rx: mpsc::Receiver<Range<BlockNumber>>,
        cutoff: BlockNumber,
    ) {
        let mut processed = 0;
        let mut discarded = 0;

        // Process all buffered messages
        while let Ok(range) = buffer_rx.try_recv() {
            if range.start >= cutoff {
                self.send_to_subscriber(Ok(range)).await;
                processed += 1;
            } else {
                discarded += 1;
            }
        }

        info!("Processed buffered messages: {} forwarded, {} discarded", processed, discarded);
    }

    async fn connect_websocket(
        provider: P,
    ) -> Result<SubResultStream<<N as Network>::HeaderResponse>, SubscriptionError> {
        let ws_stream = provider
            .subscribe_blocks()
            .await
            .map_err(|_| SubscriptionError::WebSocketConnectionFailed(1))?
            .into_result_stream();

        Ok(ws_stream)
    }

    async fn send_to_subscriber(&mut self, result: Result<Range<BlockNumber>, SubscriptionError>) {
        if let Some(ref sender) = self.subscriber {
            if sender.send(result).await.is_err() {
                warn!("Subscriber channel closed, cleaning up");
                self.subscriber = None;
                self.websocket_connected = false;
            } else {
                self.processed_count += 1;
            }
        }
    }

    async fn handle_unsubscribe(&mut self) -> Result<(), SubscriptionError> {
        if self.subscriber.is_some() {
            info!("Unsubscribing current subscriber");
            self.subscriber = None;
            self.websocket_connected = false;
        }
        Ok(())
    }

    fn get_status(&self) -> ServiceStatus {
        ServiceStatus {
            is_subscribed: self.subscriber.is_some(),
            last_synced_point: self.last_synced_point,
            websocket_connected: self.websocket_connected,
            processed_count: self.processed_count,
            error_count: self.error_count,
        }
    }

    fn current_timestamp() -> Timestamp {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
    }
}

pub struct SubscriptionClient {
    command_sender: mpsc::Sender<Command>,
}

impl SubscriptionClient {
    pub fn new(command_sender: mpsc::Sender<Command>) -> Self {
        Self { command_sender }
    }

    pub async fn subscribe(
        &self,
        from_point: Option<Timestamp>,
    ) -> Result<mpsc::Receiver<Result<Value, SubscriptionError>>, SubscriptionError> {
        let (data_tx, data_rx) = mpsc::channel(CHANNEL_BUFFER_SIZE);
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::Subscribe { sender: data_tx, from_point, response: response_tx };

        self.command_sender.send(command).await.map_err(|_| SubscriptionError::ServiceShutdown)?;

        response_rx.await.map_err(|_| SubscriptionError::ServiceShutdown)??;

        Ok(data_rx)
    }

    pub async fn unsubscribe(&self) -> Result<(), SubscriptionError> {
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::Unsubscribe { response: response_tx };

        self.command_sender.send(command).await.map_err(|_| SubscriptionError::ServiceShutdown)?;

        response_rx.await.map_err(|_| SubscriptionError::ServiceShutdown)?
    }

    pub async fn get_status(&self) -> Result<ServiceStatus, SubscriptionError> {
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::GetStatus { response: response_tx };

        self.command_sender.send(command).await.map_err(|_| SubscriptionError::ServiceShutdown)?;

        response_rx.await.map_err(|_| SubscriptionError::ServiceShutdown)
    }

    pub async fn shutdown(&self) -> Result<(), SubscriptionError> {
        let (response_tx, response_rx) = oneshot::channel();

        let command = Command::Shutdown { response: response_tx };

        self.command_sender.send(command).await.map_err(|_| SubscriptionError::ServiceShutdown)?;

        response_rx.await.map_err(|_| SubscriptionError::ServiceShutdown)
    }
}

#[derive(Debug)]
pub enum BlockScannerError {
    ErrEOF,
    ErrContinue,
    TerminalError(u64),
    EndHeightSmallerThanStartHeight(BlockNumberOrTag, BlockNumberOrTag),
    NonExistentStartHeader(BlockNumberOrTag),
    NonExistentEndHeader(BlockNumberOrTag),
    Rpc(RpcError<TransportErrorKind>),
}

impl From<RpcError<TransportErrorKind>> for BlockScannerError {
    fn from(value: RpcError<TransportErrorKind>) -> Self {
        BlockScannerError::Rpc(value)
    }
}

impl std::fmt::Display for BlockScannerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlockScannerError::ErrEOF => write!(f, "end of block batch iterator"),
            BlockScannerError::ErrContinue => write!(f, "continue"),
            BlockScannerError::TerminalError(height) => {
                write!(f, "terminal error at block height {height}")
            }
            BlockScannerError::EndHeightSmallerThanStartHeight(start, end) => {
                write!(f, "start height ({start}) > end height ({end})")
            }
            BlockScannerError::NonExistentStartHeader(height) => {
                write!(f, "failed to get start header, height: {height}")
            }
            BlockScannerError::NonExistentEndHeader(height) => {
                write!(f, "failed to get end header, height: {height}")
            }
            BlockScannerError::Rpc(err) => err.fmt(f),
        }
    }
}

#[derive(Debug)]
pub enum StartError {
    BlockScannerError(BlockScannerError),
    SendError(SendError<Result<Range<BlockNumber>, BlockScannerError>>),
}

impl From<SendError<Result<Range<BlockNumber>, BlockScannerError>>> for StartError {
    fn from(value: SendError<Result<Range<BlockNumber>, BlockScannerError>>) -> Self {
        StartError::SendError(value)
    }
}

impl From<BlockScannerError> for StartError {
    fn from(value: BlockScannerError) -> Self {
        StartError::BlockScannerError(value)
    }
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::BlockScannerError(err) => err.fmt(f),
            StartError::SendError(err) => err.fmt(f),
        }
    }
}

type EndIterFunc = fn();
type UpdateCurrentFunc = fn(Header);
pub type OnBlocksFunc<N> =
    fn(<N as Network>::BlockResponse, UpdateCurrentFunc, EndIterFunc) -> anyhow::Result<()>;

pub struct BlockScannerBuilder<N: Network> {
    blocks_read_per_epoch: usize,
    start_height: BlockNumberOrTag,
    end_height: Option<BlockNumberOrTag>,
    on_blocks: OnBlocksFunc<N>,
    reorg_rewind_depth: u64,
    retry_interval: Duration,
    block_confirmations: u64,
}

impl<N: Network> Default for BlockScannerBuilder<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N: Network> BlockScannerBuilder<N> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            blocks_read_per_epoch: DEFAULT_BLOCKS_READ_PER_EPOCH,
            start_height: BlockNumberOrTag::Latest,
            end_height: None,
            on_blocks: |_, _, _| Ok(()),
            reorg_rewind_depth: DEFAULT_REORG_REWIND_DEPTH,
            retry_interval: DEFAULT_RETRY_INTERVAL,
            block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS,
        }
    }

    #[must_use]
    pub fn with_blocks_read_per_epoch(&mut self, blocks_read_per_epoch: usize) -> &mut Self {
        self.blocks_read_per_epoch = blocks_read_per_epoch;
        self
    }

    #[must_use]
    pub fn with_start_height(&mut self, start_height: BlockNumberOrTag) -> &mut Self {
        self.start_height = start_height;
        self
    }

    #[must_use]
    pub fn with_end_height(&mut self, end_height: BlockNumberOrTag) -> &mut Self {
        self.end_height = Some(end_height);
        self
    }

    #[must_use]
    pub fn with_on_blocks(&mut self, on_blocks: OnBlocksFunc<N>) -> &mut Self {
        self.on_blocks = on_blocks;
        self
    }

    #[must_use]
    pub fn with_reorg_rewind_depth(&mut self, reorg_rewind_depth: u64) -> &mut Self {
        self.reorg_rewind_depth = reorg_rewind_depth;
        self
    }

    #[must_use]
    pub fn with_retry_interval(&mut self, retry_interval: Duration) -> &mut Self {
        self.retry_interval = retry_interval;
        self
    }

    #[must_use]
    pub fn with_block_confirmations(&mut self, block_confirmations: u64) -> &mut Self {
        self.block_confirmations = block_confirmations;
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
    ) -> Result<BlockScanner<RootProvider<N>, N>, BlockScannerError> {
        let client = ClientBuilder::default().ws(connect).await?;
        self.connect_client(client).await
    }

    /// Connects to the provider via IPC
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails
    pub async fn connect_ipc<T>(
        self,
        connect: IpcConnect<T>,
    ) -> Result<BlockScanner<RootProvider<N>, N>, BlockScannerError>
    where
        IpcConnect<T>: PubSubConnect,
    {
        let client = ClientBuilder::default().ipc(connect).await?;
        self.connect_client(client).await
    }

    #[must_use]
    pub async fn connect_client(
        self,
        client: RpcClient,
    ) -> Result<BlockScanner<RootProvider<N>, N>, BlockScannerError> {
        let provider = RootProvider::new(client);
        self.connect_provider(provider).await
    }

    pub async fn connect_provider<P>(
        self,
        provider: P,
    ) -> Result<BlockScanner<P, N>, BlockScannerError>
    where
        P: Provider<N>,
    {
        if let Some(end_height) = self.end_height {
            match (self.start_height, end_height) {
                (_, BlockNumberOrTag::Latest) => (),
                (_, BlockNumberOrTag::Number(end))
                    if end == provider.get_block_number().await? =>
                {
                    ()
                }
                (_, BlockNumberOrTag::Number(end)) if end > provider.get_block_number().await? => {
                    return Err(BlockScannerError::NonExistentEndHeader(end_height));
                }
                (BlockNumberOrTag::Number(start), BlockNumberOrTag::Number(end)) => {
                    if start > end {
                        return Err(BlockScannerError::EndHeightSmallerThanStartHeight(
                            self.start_height,
                            end_height,
                        ));
                    }
                    let start_block_number =
                        provider.get_block_number_by_id(self.start_height.into()).await?;
                    if start_block_number.is_none() {
                        return Err(BlockScannerError::NonExistentStartHeader(self.start_height));
                    }
                }
                // TODO: handle other cases
                _ => {}
            };
        }

        let (start_block, end_height) = match (self.start_height, self.end_height) {
            (_, Some(end_height)) => {
                let start_block = provider
                    .get_block_by_number(self.start_height)
                    .await?
                    .expect("already checked");
                let end_height_number = provider.get_block_number_by_id(end_height.into()).await?;
                (start_block, end_height_number)
            }
            (_, None) => {
                let start_block = provider
                    .get_block_by_number(self.start_height)
                    .await?
                    .expect("already checked");
                (start_block, None)
            }
        };

        let start_header = start_block.header();

        Ok(BlockScanner {
            provider,
            current: BlockHashAndNumber::from_header::<N>(start_header),
            is_end: false,
            blocks_read_per_epoch: self.blocks_read_per_epoch,
            start_height: start_header.number(),
            end_height,
            on_blocks: self.on_blocks,
            reorg_rewind_depth: self.reorg_rewind_depth,
            retry_interval: self.retry_interval,
            block_confirmations: self.block_confirmations,
            network: PhantomData,
        })
    }
}

struct BlockHashAndNumber {
    hash: BlockHash,
    number: BlockNumber,
}

impl BlockHashAndNumber {
    fn from_header<N: Network>(header: &N::HeaderResponse) -> Self {
        Self { hash: header.hash(), number: header.number() }
    }
}

// BlockScanner iterates the blocks in batches between the given start and end heights,
// with the awareness of reorganization.
pub struct BlockScanner<P: Provider<N>, N: Network> {
    provider: P,
    blocks_read_per_epoch: usize,
    start_height: BlockNumber,
    end_height: Option<BlockNumber>,
    current: BlockHashAndNumber,
    on_blocks: OnBlocksFunc<N>,
    is_end: bool,
    reorg_rewind_depth: u64,
    retry_interval: Duration,
    block_confirmations: u64,
    network: PhantomData<fn() -> N>,
}

impl<P, N> BlockScanner<P, N>
where
    P: Provider<N>,
    N: Network,
{
    pub async fn start(
        &mut self,
    ) -> Result<ReceiverStream<Result<Range<u64>, BlockScannerError>>, StartError> {
        let (sender, receiver) =
            mpsc::channel::<Result<Range<u64>, BlockScannerError>>(self.blocks_read_per_epoch);

        let receiver_stream = ReceiverStream::new(receiver);

        match (self.start_height, self.end_height) {
            (_, Some(end_height)) => {
                self.ensure_current_not_reorged().await?;

                sender.send(Ok(self.start_height..end_height)).await?;
                sender.send(Err(BlockScannerError::ErrEOF {})).await?;
            }
            _ => {}
        }

        tokio::spawn(
            async move { if sender.send(Err(BlockScannerError::ErrEOF {})).await.is_err() {} },
        );

        Ok(receiver_stream)
    }

    async fn ensure_current_not_reorged(&mut self) -> Result<(), BlockScannerError> {
        let current_block = self.provider.get_block_by_hash(self.current.hash).await?;
        if current_block.is_some() {
            return Ok(());
        }

        self.rewind_on_reorg_detected().await
    }

    async fn rewind_on_reorg_detected(&mut self) -> Result<(), BlockScannerError> {
        let mut new_current_height = if self.current.number <= self.reorg_rewind_depth {
            0
        } else {
            self.current.number - self.reorg_rewind_depth
        };

        let head = self.provider.get_block_number().await?;
        if head < new_current_height {
            new_current_height = head;
        }

        let current = self
            .provider
            .get_block_by_number(new_current_height.into())
            .await?
            .map(|block| BlockHashAndNumber::from_header::<N>(&block.header()))
            .expect("block should exist");

        println!(
            "Rewind on reorg detected\noldCurrent: {}, newCurrent: {}",
            self.current.number, current.number
        );

        self.current = current;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::network::{Ethereum, Network};
    use alloy_node_bindings::Anvil;
    use tokio_stream::StreamExt;

    #[allow(clippy::unnecessary_wraps)]
    fn no_op_on_blocks<N: Network>(
        _block: <N as Network>::BlockResponse,
        _update_current: UpdateCurrentFunc,
        _end_iter: EndIterFunc,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    #[test]
    fn test_block_scanner_error_display() {
        assert_eq!(format!("{}", BlockScannerError::ErrEOF), "end of block batch iterator");
        assert_eq!(format!("{}", BlockScannerError::ErrContinue), "continue");
        assert_eq!(
            format!("{}", BlockScannerError::TerminalError(42)),
            "terminal error at block height 42"
        );
    }

    #[test]
    fn test_builder_defaults() {
        let builder = BlockScannerBuilder::<Ethereum>::new();
        assert_eq!(builder.blocks_read_per_epoch, DEFAULT_BLOCKS_READ_PER_EPOCH);
        assert!(matches!(builder.start_height, BlockNumberOrTag::Latest));
        assert!(builder.end_height.is_none());
        assert_eq!(builder.reorg_rewind_depth, DEFAULT_REORG_REWIND_DEPTH);
        assert_eq!(builder.retry_interval, DEFAULT_RETRY_INTERVAL);
        assert_eq!(builder.block_confirmations, DEFAULT_BLOCK_CONFIRMATIONS);
    }

    #[test]
    fn test_builder_setters() {
        let mut builder = BlockScannerBuilder::<Ethereum>::new();
        builder.with_blocks_read_per_epoch(25);
        builder.with_start_height(BlockNumberOrTag::Earliest);
        builder.with_end_height(BlockNumberOrTag::Latest);
        builder.with_on_blocks(no_op_on_blocks::<Ethereum>);
        builder.with_reorg_rewind_depth(5);
        let interval = Duration::from_secs(3);
        builder.with_retry_interval(interval);
        builder.with_block_confirmations(12);

        assert_eq!(builder.blocks_read_per_epoch, 25);
        assert!(matches!(builder.start_height, BlockNumberOrTag::Earliest));
        assert!(matches!(builder.end_height, Some(BlockNumberOrTag::Latest)));
        assert_eq!(builder.reorg_rewind_depth, 5);
        assert_eq!(builder.retry_interval, interval);
        assert_eq!(builder.block_confirmations, 12);
    }

    #[tokio::test]
    async fn test_connect_ws_and_start_stream_eof() {
        let anvil = Anvil::new().try_spawn().expect("failed to spawn anvil");
        let ws = WsConnect::new(anvil.ws_endpoint_url());

        let builder = BlockScannerBuilder::<Ethereum>::new();
        let mut scanner = builder.connect_ws(ws).await.expect("failed to connect ws");

        let mut stream = scanner.start().await.unwrap();
        let first = stream.next().await;
        match first {
            Some(Err(BlockScannerError::ErrEOF)) => {}
            other => panic!("expected first stream item to be ErrEOF, got: {other:?}"),
        }
    }
}
