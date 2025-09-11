use std::{marker::PhantomData, time::Duration};

use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio_stream::wrappers::ReceiverStream;

use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    providers::{Provider, RootProvider},
    pubsub::PubSubConnect,
    rpc::{
        client::{ClientBuilder, RpcClient},
        types::Header,
    },
    transports::{TransportError, ipc::IpcConnect, ws::WsConnect},
};

// copied form https://github.com/taikoxyz/taiko-mono/blob/f4b3a0e830e42e2fee54829326389709dd422098/packages/taiko-client/pkg/chain_iterator/block_batch_iterator.go#L19
const DEFAULT_BLOCKS_READ_PER_EPOCH: u64 = 1000;
const DEFAULT_RETRY_INTERVAL: Duration = Duration::from_secs(12);
const DEFAULT_BLOCK_CONFIRMATIONS: u64 = 0;
const BACK_OFF_MAX_RETRIES: u64 = 5;

// TODO: determine check exact default value
const DEFAULT_REORG_REWIND_DEPTH: u64 = 0;

// State sync aware retry settings
const STATE_SYNC_RETRY_INTERVAL: Duration = Duration::from_secs(30);
const STATE_SYNC_MAX_RETRIES: u64 = 12;

#[derive(Debug)]
pub enum BlockScannerError {
    ErrEOF,
    ErrContinue,
    TerminalError(u64),
}

impl std::fmt::Display for BlockScannerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlockScannerError::ErrEOF => write!(f, "end of block batch iterator"),
            BlockScannerError::ErrContinue => write!(f, "continue"),
            BlockScannerError::TerminalError(height) => {
                write!(f, "terminal error at block height {}", height)
            }
        }
    }
}

type EndIterFunc = fn();
type UpdateCurrentFunc = fn(Header);
pub type OnBlocksFunc<N> =
    fn(<N as Network>::BlockResponse, UpdateCurrentFunc, EndIterFunc) -> anyhow::Result<()>;

pub struct BlockScannerBuilder<N: Network> {
    blocks_read_per_epoch: u64,
    start_height: BlockNumberOrTag,
    end_height: BlockNumberOrTag,
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
    pub fn new() -> Self {
        Self {
            blocks_read_per_epoch: DEFAULT_BLOCKS_READ_PER_EPOCH,
            start_height: BlockNumberOrTag::Earliest,
            end_height: BlockNumberOrTag::Latest,
            on_blocks: |_, _, _| Ok(()),
            reorg_rewind_depth: DEFAULT_REORG_REWIND_DEPTH,
            retry_interval: DEFAULT_RETRY_INTERVAL,
            block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS,
        }
    }

    pub fn with_blocks_read_per_epoch(&mut self, blocks_read_per_epoch: u64) -> &mut Self {
        // TODO: Clamp this value to a reasonable range
        self.blocks_read_per_epoch = blocks_read_per_epoch;
        self
    }

    pub fn with_start_height(&mut self, start_height: BlockNumberOrTag) -> &mut Self {
        self.start_height = start_height;
        self
    }

    pub fn with_end_height(&mut self, end_height: BlockNumberOrTag) -> &mut Self {
        self.end_height = end_height;
        self
    }

    pub fn with_on_blocks(&mut self, on_blocks: OnBlocksFunc<N>) -> &mut Self {
        self.on_blocks = on_blocks;
        self
    }

    pub fn with_reorg_rewind_depth(&mut self, reorg_rewind_depth: u64) -> &mut Self {
        self.reorg_rewind_depth = reorg_rewind_depth;
        self
    }

    pub fn with_retry_interval(&mut self, retry_interval: Duration) -> &mut Self {
        self.retry_interval = retry_interval;
        self
    }

    pub fn with_block_confirmations(&mut self, block_confirmations: u64) -> &mut Self {
        self.block_confirmations = block_confirmations;
        self
    }

    pub async fn connect_ws(
        self,
        connect: WsConnect,
    ) -> Result<BlockScanner<RootProvider<N>, N>, TransportError> {
        let client = ClientBuilder::default().ws(connect).await?;
        Ok(self.connect_client(client))
    }

    pub async fn connect_ipc<T>(
        self,
        connect: IpcConnect<T>,
    ) -> Result<BlockScanner<RootProvider<N>, N>, TransportError>
    where
        IpcConnect<T>: PubSubConnect,
    {
        let client = ClientBuilder::default().ipc(connect).await?;
        Ok(self.connect_client(client))
    }

    pub fn connect_client(self, client: RpcClient) -> BlockScanner<RootProvider<N>, N> {
        let provider = RootProvider::new(client);
        self.connect_provider(provider)
    }

    pub fn connect_provider<P>(self, provider: P) -> BlockScanner<P, N>
    where
        P: Provider<N>,
    {
        let (sender, receiver) = mpsc::channel(self.blocks_read_per_epoch.try_into().unwrap());

        BlockScanner {
            provider,
            sender,
            receiver,
            current: Header::default(),
            is_end: false,
            blocks_read_per_epoch: self.blocks_read_per_epoch,
            start_height: self.start_height,
            end_height: self.end_height,
            on_blocks: self.on_blocks,
            reorg_rewind_depth: self.reorg_rewind_depth,
            retry_interval: self.retry_interval,
            block_confirmations: self.block_confirmations,
            network: PhantomData,
        }
    }
}

// BlockScanner iterates the blocks in batches between the given start and end heights,
// with the awareness of reorganization.
pub struct BlockScanner<P: Provider<N>, N: Network> {
    provider: P,
    sender: Sender<Result<N::BlockResponse, BlockScannerError>>,
    receiver: Receiver<Result<N::BlockResponse, BlockScannerError>>,
    blocks_read_per_epoch: u64,
    start_height: BlockNumberOrTag,
    end_height: BlockNumberOrTag,
    current: Header,
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
    pub async fn start(self) -> ReceiverStream<Result<N::BlockResponse, BlockScannerError>> {
        let receiver_stream = ReceiverStream::new(self.receiver);

        tokio::spawn(async move {
            if self.sender.send(Err(BlockScannerError::ErrEOF {})).await.is_err() {}
        });

        receiver_stream
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::network::{Ethereum, Network};
    use alloy_node_bindings::Anvil;
    use tokio_stream::StreamExt;

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
        assert!(matches!(builder.start_height, BlockNumberOrTag::Earliest));
        assert!(matches!(builder.end_height, BlockNumberOrTag::Latest));
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
        assert!(matches!(builder.end_height, BlockNumberOrTag::Latest));
        assert_eq!(builder.reorg_rewind_depth, 5);
        assert_eq!(builder.retry_interval, interval);
        assert_eq!(builder.block_confirmations, 12);
    }

    #[tokio::test]
    async fn test_connect_ws_and_start_stream_eof() {
        let anvil = Anvil::new().try_spawn().expect("failed to spawn anvil");
        let ws = WsConnect::new(anvil.ws_endpoint_url());

        let builder = BlockScannerBuilder::<Ethereum>::new();
        let scanner = builder.connect_ws(ws).await.expect("failed to connect ws");

        let mut stream = scanner.start().await;
        let first = stream.next().await;
        match first {
            Some(Err(BlockScannerError::ErrEOF)) => {}
            other => panic!("expected first stream item to be ErrEOF, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_channel_buffer_is_equal_to_blocks_read_per_epoch() {
        let anvil = Anvil::new().try_spawn().expect("failed to spawn anvil");
        let ws = WsConnect::new(anvil.ws_endpoint_url());

        let mut builder = BlockScannerBuilder::<Ethereum>::new();
        builder.with_blocks_read_per_epoch(5);

        let scanner = builder.connect_ws(ws).await.expect("failed to connect ws");

        for _ in 0..scanner.blocks_read_per_epoch {
            scanner
                .sender
                .try_send(Err(BlockScannerError::ErrContinue))
                .expect("channel should not be full yet");
        }

        let res = scanner.sender.try_send(Err(BlockScannerError::ErrContinue));
        assert!(matches!(res, Err(tokio::sync::mpsc::error::TrySendError::Full(_))));
    }
}
