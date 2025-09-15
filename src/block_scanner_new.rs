//! Example usage:
//!
//! ```rust,no_run
//! use alloy::primitives::BlockNumber;
//! use alloy::eips::BlockNumberOrTag;
//! use std::ops::Range;
//! use tracing::{debug, error, info, warn};
//!
//! pub struct DataProcessor {
//!     processed_count: u64,
//!     last_range: Option<Range<BlockNumber>>,
//!     client: SubscriptionClient,
//! }
//!
//! impl DataProcessor {
//!     pub fn new(client: SubscriptionClient) -> Self {
//!         Self {
//!             processed_count: 0,
//!             last_range: None,
//!             client,
//!         }
//!     }
//!
//!     pub async fn start_processing(
//!         &mut self,
//!         start_height: BlockNumberOrTag,
//!         end_height: Option<BlockNumberOrTag>,
//!         buffer_size: usize,
//!     ) -> Result<(), SubscriptionError> {
//!         info!("Starting data processing from point: {:?}", start_height);
//!     
//!         let mut data_receiver = self.client.subscribe(
//!             start_height,
//!             end_height,
//!             buffer_size,
//!         ).await?;
//!     
//!         while let Some(result) = data_receiver.recv().await {
//!             match result {
//!                 Ok(range) => {
//!                     if let Err(e) = self.process_range(range).await {
//!                         error!("Error processing block range: {}", e);
//!                     }
//!                 }
//!                 Err(e) => {
//!                     error!("Received error from subscription: {}", e);
//!                 
//!                     // Decide whether to continue or break based on error type
//!                     match e {
//!                         SubscriptionError::ServiceShutdown => break,
//!                         SubscriptionError::WebSocketConnectionFailed(_) => {
//!                             // Maybe implement backoff and retry logic here
//!                             warn!("WebSocket connection failed, continuing to listen for reconnection");
//!                         }
//!                         _ => {
//!                             // Continue processing for other errors
//!                             warn!("Non-fatal error, continuing: {}", e);
//!                         }
//!                     }
//!                 }
//!             }
//!         }
//!     
//!         info!("Data processing stopped. Processed {} values", self.processed_count);
//!         Ok(())
//!     }
//!
//!     async fn process_range(&mut self, range: Range<BlockNumber>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
//!         // Your application-specific processing logic here
//!         debug!("Processing range: {} to {}", range.start, range.end);
//!     
//!         // Example processing: detect significant changes
//!         if let Some(ref last) = self.last_range {
//!             if range.end - last.end > 10 { // More than 10 blocks gap
//!                 warn!("Large block gap detected: {} -> {}", last.end, range.end);
//!             }
//!         }
//!     
//!         // TODO: fetch events from range of blocks and process them
//!     
//!         self.processed_count += 1;
//!         self.last_range = Some(range);
//!     
//!         // Periodic logging
//!         if self.processed_count % 1000 == 0 {
//!             info!("Processed {} ranges", self.processed_count);
//!         }
//!     
//!         Ok(())
//!     }
//!
//!     pub async fn stop(&self) -> Result<(), SubscriptionError> {
//!         self.client.unsubscribe().await
//!     }
//!
//!     pub async fn get_stats(&self) -> Result<(u64, ServiceStatus), SubscriptionError> {
//!         let service_status = self.client.get_status().await?;
//!         Ok((self.processed_count, service_status))
//!     }
//! }
//!
//! // ============================================================================
//! // Example Usage
//! // ============================================================================
//!
//! use tokio::time::Duration;
//! use event_scanner::block_scanner_new::{SubscriptionService, SubscriptionClient, Config, ServiceStatus, SubscriptionError};
//! use alloy::transports::http::reqwest::Url;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Initialize logging
//!     tracing_subscriber::fmt::init();
//!
//!     // Configuration
//!     let config = Config {
//!         ws_url: Url::parse("ws://localhost:8546").unwrap(),
//!         blocks_read_per_epoch: 1000,
//!         reorg_rewind_depth: 5,
//!         retry_interval: Duration::from_secs(12),
//!         block_confirmations: 5,
//!     };
//!
//!     // Start the subscription service
//!     let (service, command_sender) = SubscriptionService::new(config);
//!     tokio::spawn(async move {
//!         service.run::<alloy::network::Ethereum>().await;
//!     });
//!
//!     // Create client and data processor
//!     let client = SubscriptionClient::new(command_sender);
//!     let mut processor = DataProcessor::new(client);
//!
//!     // Just subscribe
//!     processor.start_processing(
//!         BlockNumberOrTag::Latest,
//!         None, // just subscribe to new blocks
//!         1, // wait until current blocks are processed before processing next range
//!     ).await?;
//!
//!     Ok(())
//! }
//! ```

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
        RpcError, TransportError, TransportErrorKind,
        http::reqwest::{self, Url},
        ipc::IpcConnect,
        ws::{self, WsConnect},
    },
};
use thiserror::Error;

use tracing::{debug, error, info, warn};

// copied form https://github.com/taikoxyz/taiko-mono/blob/f4b3a0e830e42e2fee54829326389709dd422098/packages/taiko-client/pkg/chain_iterator/block_batch_iterator.go#L19
const DEFAULT_BLOCKS_READ_PER_EPOCH: usize = 1000;
const DEFAULT_RETRY_INTERVAL: Duration = Duration::from_secs(12);
const DEFAULT_BLOCK_CONFIRMATIONS: u64 = 0;
const BACK_OFF_MAX_RETRIES: u64 = 5;

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
pub enum Command {
    Subscribe {
        sender: mpsc::Sender<Result<Range<BlockNumber>, SubscriptionError>>,
        start_height: BlockNumberOrTag,
        end_height: Option<BlockNumberOrTag>,
        response: oneshot::Sender<Result<(), SubscriptionError>>,
    },
    Unsubscribe {
        response: oneshot::Sender<Result<(), SubscriptionError>>,
    },
    GetStatus {
        response: oneshot::Sender<ServiceStatus>,
    },
    Shutdown {
        response: oneshot::Sender<()>,
    },
}

#[derive(Debug, Clone)]
pub struct ServiceStatus {
    pub is_subscribed: bool,
    pub last_synced_block: Option<BlockHashAndNumber>,
    pub websocket_connected: bool,
    pub processed_count: u64,
    pub error_count: u64,
}

#[derive(Debug, Clone)]
pub struct BlockHashAndNumber {
    hash: BlockHash,
    number: BlockNumber,
}

impl BlockHashAndNumber {
    fn from_header<N: Network>(header: &N::HeaderResponse) -> Self {
        Self { hash: header.hash(), number: header.number() }
    }
}

pub struct Config {
    pub ws_url: Url,
    pub blocks_read_per_epoch: usize,
    pub reorg_rewind_depth: u64,
    pub retry_interval: Duration,
    pub block_confirmations: u64,
}

pub struct SubscriptionService {
    config: Config,
    subscriber: Option<mpsc::Sender<Result<Range<BlockNumber>, SubscriptionError>>>,
    last_synced_block: Option<BlockHashAndNumber>,
    websocket_connected: bool,
    processed_count: u64,
    error_count: u64,
    command_receiver: mpsc::Receiver<Command>,
    shutdown: bool,
}

impl SubscriptionService {
    pub fn new(config: Config) -> (Self, mpsc::Sender<Command>) {
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

    pub async fn run<N: Network>(mut self) {
        info!("Starting subscription service");

        while !self.shutdown {
            tokio::select! {
                cmd = self.command_receiver.recv() => {
                    match cmd {
                        Some(command) => {
                            if let Err(e) = self.handle_command::<N>(command).await {
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

    async fn handle_command<N: Network>(
        &mut self,
        command: Command,
    ) -> Result<(), SubscriptionError> {
        match command {
            Command::Subscribe { sender, start_height, end_height, response } => {
                let result = self.handle_subscribe::<N>(sender, start_height, end_height).await;
                let _ = response.send(result);
            }
            Command::Unsubscribe { response } => {
                let result = self.handle_unsubscribe().await;
                let _ = response.send(result);
            }
            Command::GetStatus { response } => {
                let status = self.get_status();
                let _ = response.send(status);
            }
            Command::Shutdown { response } => {
                self.shutdown = true;
                self.handle_unsubscribe().await?;
                let _ = response.send(());
            }
        }
        Ok(())
    }

    async fn handle_subscribe<N: Network>(
        &mut self,
        sender: mpsc::Sender<Result<Range<BlockNumber>, SubscriptionError>>,
        start_height: BlockNumberOrTag,
        end_height: Option<BlockNumberOrTag>,
    ) -> Result<(), SubscriptionError> {
        if self.subscriber.is_some() {
            return Err(SubscriptionError::MultipleSubscribers);
        }

        // TODO: update local state relate to reorg and validate data

        info!("Starting subscription from point: {:?}", start_height);
        self.subscriber = Some(sender);

        self.sync_with_transition::<N>(start_height, end_height).await?;

        Ok(())
    }

    async fn sync_with_transition<N: Network>(
        &mut self,
        start_height: BlockNumberOrTag,
        end_height: Option<BlockNumberOrTag>,
    ) -> Result<(), SubscriptionError> {
        // Step 1: Establish WebSocket connection and start buffering
        let (buffer_tx, buffer_rx) = mpsc::channel(MAX_BUFFERED_MESSAGES);

        let connect = WsConnect::new(self.config.ws_url.clone());
        let provider = RootProvider::<N>::new(ClientBuilder::default().ws(connect).await?);
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
        if let Err(e) = self
            .sync_historical_data(&provider, start_block, sync_end_block.header().number())
            .await
        {
            ws_task.abort();
            return Err(SubscriptionError::HistoricalSyncError(e.to_string()));
        }

        // Step 3: Process buffered WebSocket messages
        let cutoff = sync_end_block.header().number();
        let sender = self.subscriber.clone().expect("subscriber should be set");
        tokio::spawn(async move {
            Self::process_buffered_messages(buffer_rx, sender, cutoff).await;
        });

        self.last_synced_block =
            Some(BlockHashAndNumber::from_header::<N>(sync_end_block.header()));

        info!("Successfully transitioned from historical to live data");
        Ok(())
    }

    async fn sync_historical_data<P: Provider<N>, N: Network>(
        &mut self,
        provider: &P,
        from: BlockNumber,
        to: BlockNumber,
    ) -> Result<(), SubscriptionError> {
        let mut batch_count = 0;

        while self.last_synced_block.as_ref().unwrap().number < to {
            self.ensure_current_not_reorged(provider).await?;

            let batch_to = if self.last_synced_block.as_ref().unwrap().number
                + self.config.blocks_read_per_epoch as u64
                > to
            {
                to
            } else {
                self.last_synced_block.as_ref().unwrap().number
                    + self.config.blocks_read_per_epoch as u64
            };

            let batch_end_block = provider
                .get_block_by_number(batch_to.into())
                .await?
                .expect("TODO: check if really valid");

            self.send_to_subscriber(Ok(self.last_synced_block.as_ref().unwrap().number..batch_to))
                .await;

            self.last_synced_block =
                Some(BlockHashAndNumber::from_header::<N>(&batch_end_block.header()));

            batch_count += 1;
            if batch_count % 10 == 0 {
                debug!("Processed {} historical batches", batch_count);
            }
        }

        info!("Historical sync completed: {} batches processed", batch_count);
        Ok(())
    }

    async fn ensure_current_not_reorged<P: Provider<N>, N: Network>(
        &mut self,
        provider: &P,
    ) -> Result<(), SubscriptionError> {
        let current_block =
            provider.get_block_by_hash(self.last_synced_block.as_ref().unwrap().hash).await?;
        if current_block.is_some() {
            return Ok(());
        }

        self.rewind_on_reorg_detected(provider).await
    }

    async fn rewind_on_reorg_detected<P: Provider<N>, N: Network>(
        &mut self,
        provider: P,
    ) -> Result<(), SubscriptionError> {
        let mut new_current_height =
            if self.last_synced_block.as_ref().unwrap().number <= self.config.reorg_rewind_depth {
                0
            } else {
                self.last_synced_block.as_ref().unwrap().number - self.config.reorg_rewind_depth
            };

        let head = provider.get_block_number().await?;
        if head < new_current_height {
            new_current_height = head;
        }

        let current = provider
            .get_block_by_number(new_current_height.into())
            .await?
            .map(|block| BlockHashAndNumber::from_header::<N>(&block.header()))
            .expect("block should exist");

        println!(
            "Rewind on reorg detected\noldCurrent: {}, newCurrent: {}",
            self.last_synced_block.as_ref().unwrap().number,
            current.number
        );

        self.last_synced_block = Some(current);

        Ok(())
    }

    async fn websocket_buffer_task<P: Provider<N> + Clone, N: Network>(
        provider: P,
        buffer_sender: mpsc::Sender<Range<BlockNumber>>,
    ) {
        let mut retry_count = 0;

        loop {
            match Self::connect_websocket::<P, N>(provider.clone()).await {
                Ok(mut ws_stream) => {
                    info!("WebSocket connected for buffering");
                    retry_count = 0;

                    while let Some(value) = ws_stream.next().await {
                        // TODO: handle reorgs
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
        mut buffer_rx: mpsc::Receiver<Range<BlockNumber>>,
        sender: mpsc::Sender<Result<Range<BlockNumber>, SubscriptionError>>,
        cutoff: BlockNumber,
    ) {
        let mut processed = 0;
        let mut discarded = 0;

        // Process all buffered messages
        while let Ok(range) = buffer_rx.try_recv() {
            let (start, end) = (range.start, range.end);
            if start > cutoff {
                if sender.send(Ok(range)).await.is_err() {
                    warn!("Subscriber channel closed, cleaning up");
                    return;
                }
                processed += end - start;
            } else if end > cutoff + 1 {
                // TODO: verify the math
                discarded += cutoff - start;

                let start = cutoff + 1;
                if sender.send(Ok((start..end))).await.is_err() {
                    warn!("Subscriber channel closed, cleaning up");
                    return;
                }
                processed += end - start;
            } else {
                discarded += end - start;
            }
        }

        info!("Processed buffered messages: {} forwarded, {} discarded", processed, discarded);
    }

    async fn connect_websocket<P: Provider<N>, N: Network>(
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
            last_synced_block: self.last_synced_block.clone(),
            websocket_connected: self.websocket_connected,
            processed_count: self.processed_count,
            error_count: self.error_count,
        }
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
        start_height: BlockNumberOrTag,
        end_height: Option<BlockNumberOrTag>,
        buffer_size: usize,
    ) -> Result<mpsc::Receiver<Result<Range<BlockNumber>, SubscriptionError>>, SubscriptionError>
    {
        let (data_tx, data_rx) = mpsc::channel(buffer_size);
        let (response_tx, response_rx) = oneshot::channel();

        let command =
            Command::Subscribe { sender: data_tx, start_height, end_height, response: response_tx };

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

#[cfg(test)]
mod tests {
    use alloy::{providers::ProviderBuilder, sol};
    use alloy_node_bindings::Anvil;
    use tokio::time::sleep;

    use super::*;

    sol! {
        #[allow(missing_docs)]
        #[sol(rpc, bytecode="608080604052346015576101b0908161001a8239f35b5f80fdfe6080806040526004361015610012575f80fd5b5f3560e01c90816306661abd1461016157508063a87d942c14610145578063d732d955146100ad5763e8927fbc14610048575f80fd5b346100a9575f3660031901126100a9575f5460018101809111610095576020817f7ca2ca9527391044455246730762df008a6b47bbdb5d37a890ef78394535c040925f55604051908152a1005b634e487b7160e01b5f52601160045260245ffd5b5f80fd5b346100a9575f3660031901126100a9575f548015610100575f198101908111610095576020817f53a71f16f53e57416424d0d18ccbd98504d42a6f98fe47b09772d8f357c620ce925f55604051908152a1005b60405162461bcd60e51b815260206004820152601860248201527f436f756e742063616e6e6f74206265206e6567617469766500000000000000006044820152606490fd5b346100a9575f3660031901126100a95760205f54604051908152f35b346100a9575f3660031901126100a9576020905f548152f3fea2646970667358221220b846b706f79f5ae1fc4a4238319e723a092f47ce4051404186424739164ab02264736f6c634300081e0033")]
        contract LiveTestCounter {
            uint256 public count;

            event CountIncreased(uint256 newCount);
            event CountDecreased(uint256 newCount);

            function increase() public {
                count += 1;
                emit CountIncreased(count);
            }

            function decrease() public {
                require(count > 0, "Count cannot be negative");
                count -= 1;
                emit CountDecreased(count);
            }

            function getCount() public view returns (uint256) {
                return count;
            }
        }
    }

    #[tokio::test]
    async fn test_subscription_works() -> anyhow::Result<()> {
        let anvil = Anvil::new().block_time_f64(0.05).try_spawn()?;
        let wallet = anvil.wallet();
        let provider = ProviderBuilder::new()
            .wallet(wallet.unwrap())
            .connect(anvil.endpoint().as_str())
            .await?;

        let contract = LiveTestCounter::deploy(provider.clone()).await?;
        let contract_address = *contract.address();

        let (sender, receiver) = mpsc::channel(1);
        let sub_client = SubscriptionClient::new(sender);

        let (subscription_service, cmd_tx) = SubscriptionService::new(Config {
            ws_url: anvil.ws_endpoint_url(),
            blocks_read_per_epoch: 3,
            reorg_rewind_depth: 5,
            retry_interval: Duration::from_secs(1),
            block_confirmations: 1,
        });

        let handle = tokio::spawn(async move {
            subscription_service.run::<alloy::network::Ethereum>().await;
        });

        let mut data_receiver =
            sub_client.subscribe(BlockNumberOrTag::Latest, None, 1).await.unwrap();

        let ctr_handle = tokio::spawn(async move {
            for _ in 0..10 {
                let tx = contract.increase().send().await;
                assert!(tx.is_ok());

                sleep(Duration::from_millis(100)).await;
            }
        });

        while let Some(result) = data_receiver.recv().await {
            match result {
                Ok(range) => {
                    // if let Err(e) = self.process_range(range).await {
                    //     error!("Error processing block range: {}", e);
                    // }
                }
                Err(e) => {
                    error!("Received error from subscription: {}", e);

                    // Decide whether to continue or break based on error type
                    match e {
                        SubscriptionError::ServiceShutdown => break,
                        SubscriptionError::WebSocketConnectionFailed(_) => {
                            // Maybe implement backoff and retry logic here
                            warn!(
                                "WebSocket connection failed, continuing to listen for reconnection"
                            );
                        }
                        _ => {
                            // Continue processing for other errors
                            warn!("Non-fatal error, continuing: {}", e);
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
