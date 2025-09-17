#![allow(unused)]

use std::{future, marker::PhantomData, ops::Range, time::Duration};

use tokio::sync::mpsc::{self, Receiver, Sender, error::SendError};
use tokio_stream::wrappers::ReceiverStream;

use alloy::{
    consensus::BlockHeader,
    eips::{BlockId, BlockNumberOrTag, RpcBlockHash},
    network::{BlockResponse, Network, primitives::HeaderResponse},
    primitives::{BlockHash, BlockNumber},
    providers::{Provider, RootProvider},
    pubsub::PubSubConnect,
    rpc::{
        client::{ClientBuilder, RpcClient},
        types::Header,
    },
    transports::{RpcError, TransportError, TransportErrorKind, ipc::IpcConnect, ws::WsConnect},
};

// copied form https://github.com/taikoxyz/taiko-mono/blob/f4b3a0e830e42e2fee54829326389709dd422098/packages/taiko-client/pkg/chain_iterator/block_batch_iterator.go#L19
const DEFAULT_BLOCKS_READ_PER_EPOCH: usize = 1000;
const DEFAULT_RETRY_INTERVAL: Duration = Duration::from_secs(12);
const DEFAULT_BLOCK_CONFIRMATIONS: u64 = 0;

// TODO: determine check exact default value
const DEFAULT_REORG_REWIND_DEPTH: u64 = 0;

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
    fn from_header<N: Network>(block: &N::HeaderResponse) -> Self {
        Self { hash: block.hash(), number: block.number() }
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
    pub fn provider(&self) -> &P {
        &self.provider
    }

    pub async fn start(&self) -> ReceiverStream<Result<Range<u64>, BlockScannerError>> {
        let (sender, receiver) = mpsc::channel(self.blocks_read_per_epoch);

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
