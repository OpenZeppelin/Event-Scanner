use std::{marker::PhantomData, time::Duration};

use tokio::sync::mpsc::{self, Receiver, Sender};

use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    providers::{Provider, ProviderBuilder, RootProvider},
    rpc::{
        self,
        client::{ClientBuilder, RpcClient},
        types::Header,
    },
    transports::http::{Client, Http, reqwest},
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

#[derive(Debug, Clone)]
struct ErrEOF;

#[derive(Debug, Clone)]
struct ErrContinue;

impl std::fmt::Display for ErrEOF {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "end of block batch iterator")
    }
}

impl std::fmt::Display for ErrContinue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "continue")
    }
}

type EndIterFunc = fn();
type UpdateCurrentFunc = fn(Header);
type OnBlocksFunc = fn(Header, Header, UpdateCurrentFunc, EndIterFunc) -> anyhow::Result<()>;

// BlockScanner iterates the blocks in batches between the given start and end heights,
// with the awareness of reorganization.
pub struct BlockScanner<P: Provider<N>, N: Network> {
    provider: P,
    sender: Sender<N::BlockResponse>,
    receiver: Receiver<N::BlockResponse>,
    blocks_read_per_epoch: u64,
    start_height: BlockNumberOrTag,
    end_height: BlockNumberOrTag,
    current: Header,
    on_blocks: OnBlocksFunc,
    is_end: bool,
    reorg_rewind_depth: u64,
    retry_interval: Duration,
    block_confirmations: u64,
    network: PhantomData<fn() -> N>,
}

pub struct BlockScannerBuilder {
    blocks_read_per_epoch: u64,
    start_height: BlockNumberOrTag,
    end_height: BlockNumberOrTag,
    on_blocks: OnBlocksFunc,
    reorg_rewind_depth: u64,
    retry_interval: Duration,
    block_confirmations: u64,
}

impl BlockScannerBuilder {
    pub fn new() -> Self {
        Self {
            blocks_read_per_epoch: DEFAULT_BLOCKS_READ_PER_EPOCH,
            start_height: BlockNumberOrTag::Earliest,
            end_height: BlockNumberOrTag::Latest,
            on_blocks: |_, _, _, _| Ok(()),
            reorg_rewind_depth: DEFAULT_REORG_REWIND_DEPTH,
            retry_interval: DEFAULT_RETRY_INTERVAL,
            block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS,
        }
    }

    pub fn with_blocks_read_per_epoch(&mut self, blocks_read_per_epoch: u64) -> &mut Self {
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

    pub fn with_on_blocks(&mut self, on_blocks: OnBlocksFunc) -> &mut Self {
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

    pub fn connect_http<N: Network>(
        self,
        rpc_url: reqwest::Url,
    ) -> BlockScanner<RootProvider<N>, N> {
        let client = ClientBuilder::default().http(rpc_url);
        self.connect_client(client)
    }

    pub fn connect_client<N>(self, client: RpcClient) -> BlockScanner<RootProvider<N>, N>
    where
        N: Network,
    {
        let provider = RootProvider::new(client);
        self.connect_provider(provider)
    }

    pub fn connect_provider<P, N>(self, provider: P) -> BlockScanner<P, N>
    where
        P: Provider<N>,
        N: Network,
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
