use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    primitives::{Address, B256},
    providers::{RootProvider, WsConnect},
    rpc::client::ClientBuilder,
    transports::{TransportResult, http::reqwest::Url},
};

use crate::block_range_scanner::DEFAULT_BLOCKS_READ_PER_EPOCH;

pub type Result<T> = std::result::Result<T, Error>;
pub type Error = Box<dyn std::error::Error + Send + Sync>;

pub struct EventStream;

pub struct HistoricMode {
    from_block: BlockNumberOrTag,
    to_block: BlockNumberOrTag,
    address: Address,
    events: Vec<B256>,
    max_reads: usize,
}

pub struct SubscribeMode {
    address: Address,
    events: Vec<B256>,
    block_confirmations: u64,
    max_reads: usize,
}

pub struct SyncMode {
    from_block: BlockNumberOrTag,
    address: Address,
    events: Vec<B256>,
    block_confirmations: u64,
    max_reads: usize,
}

pub struct LatestMode {
    count: u64,
    address: Address,
    events: Vec<B256>,
    max_reads: usize,
}

pub struct ConnectedHistoricMode<N: Network> {
    _provider: RootProvider<N>,
}

pub struct ConnectedSubscribeMode<N: Network> {
    _provider: RootProvider<N>,
}

pub struct ConnectedSyncMode<N: Network> {
    _provider: RootProvider<N>,
}

pub struct ConnectedLatestMode<N: Network> {
    _provider: RootProvider<N>,
}

pub struct EventScanner;

impl EventScanner {
    pub fn historic() -> HistoricMode {
        HistoricMode {
            from_block: BlockNumberOrTag::Earliest,
            to_block: BlockNumberOrTag::Latest,
            address: Address::ZERO,
            events: Vec::new(),
            max_reads: DEFAULT_BLOCKS_READ_PER_EPOCH,
        }
    }

    pub fn subscribe() -> SubscribeMode {
        SubscribeMode {
            address: Address::ZERO,
            events: Vec::new(),
            block_confirmations: 0,
            max_reads: DEFAULT_BLOCKS_READ_PER_EPOCH,
        }
    }

    pub fn sync() -> SyncMode {
        SyncMode {
            from_block: BlockNumberOrTag::Earliest,
            address: Address::ZERO,
            events: Vec::new(),
            block_confirmations: 0,
            max_reads: DEFAULT_BLOCKS_READ_PER_EPOCH,
        }
    }

    pub fn latest(count: u64) -> LatestMode {
        LatestMode {
            count,
            address: Address::ZERO,
            events: Vec::new(),
            max_reads: DEFAULT_BLOCKS_READ_PER_EPOCH,
        }
    }
}

impl HistoricMode {
    pub fn from_block(mut self, block: impl Into<BlockNumberOrTag>) -> Self {
        self.from_block = block.into();
        self
    }

    pub fn to_block(mut self, block: impl Into<BlockNumberOrTag>) -> Self {
        self.to_block = block.into();
        self
    }

    pub fn contract_address(mut self, address: Address) -> Self {
        self.address = address;
        self
    }

    pub fn event(mut self, signature: B256) -> Self {
        self.events.push(signature);
        self
    }

    pub fn events(mut self, signatures: Vec<B256>) -> Self {
        self.events.extend(signatures);
        self
    }

    pub fn max_reads(mut self, max: usize) -> Self {
        self.max_reads = max;
        self
    }

    pub async fn connect_ws<N: Network>(
        self,
        ws_url: Url,
    ) -> TransportResult<ConnectedHistoricMode<N>> {
        let provider =
            RootProvider::<N>::new(ClientBuilder::default().ws(WsConnect::new(ws_url)).await?);

        Ok(ConnectedHistoricMode { _provider: provider })
    }

    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<ConnectedHistoricMode<N>> {
        let provider = RootProvider::<N>::new(ClientBuilder::default().ipc(ipc_path.into()).await?);

        Ok(ConnectedHistoricMode { _provider: provider })
    }

    pub fn connect<N: Network>(self, provider: RootProvider<N>) -> ConnectedHistoricMode<N> {
        ConnectedHistoricMode { _provider: provider }
    }
}

impl<N: Network> ConnectedHistoricMode<N> {
    pub fn stream(self) -> EventStream {
        EventStream
    }
}

impl SubscribeMode {
    pub fn contract_address(mut self, address: Address) -> Self {
        self.address = address;
        self
    }

    pub fn event(mut self, signature: B256) -> Self {
        self.events.push(signature);
        self
    }

    pub fn events(mut self, signatures: Vec<B256>) -> Self {
        self.events.extend(signatures);
        self
    }

    pub fn block_confirmations(mut self, count: u64) -> Self {
        self.block_confirmations = count;
        self
    }

    pub fn max_reads(mut self, max: usize) -> Self {
        self.max_reads = max;
        self
    }

    pub async fn connect_ws<N: Network>(
        self,
        ws_url: Url,
    ) -> TransportResult<ConnectedSubscribeMode<N>> {
        let provider =
            RootProvider::<N>::new(ClientBuilder::default().ws(WsConnect::new(ws_url)).await?);

        Ok(ConnectedSubscribeMode { _provider: provider })
    }

    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<ConnectedSubscribeMode<N>> {
        let provider = RootProvider::<N>::new(ClientBuilder::default().ipc(ipc_path.into()).await?);

        Ok(ConnectedSubscribeMode { _provider: provider })
    }

    pub fn connect<N: Network>(self, provider: RootProvider<N>) -> ConnectedSubscribeMode<N> {
        ConnectedSubscribeMode { _provider: provider }
    }
}

impl<N: Network> ConnectedSubscribeMode<N> {
    pub fn stream(self) -> EventStream {
        EventStream
    }
}

impl SyncMode {
    pub fn from_block(mut self, block: impl Into<BlockNumberOrTag>) -> Self {
        self.from_block = block.into();
        self
    }

    pub fn contract_address(mut self, address: Address) -> Self {
        self.address = address;
        self
    }

    pub fn event(mut self, signature: B256) -> Self {
        self.events.push(signature);
        self
    }

    pub fn events(mut self, signatures: Vec<B256>) -> Self {
        self.events.extend(signatures);
        self
    }

    pub fn block_confirmations(mut self, count: u64) -> Self {
        self.block_confirmations = count;
        self
    }

    pub fn max_reads(mut self, max: usize) -> Self {
        self.max_reads = max;
        self
    }

    pub async fn connect_ws<N: Network>(
        self,
        ws_url: Url,
    ) -> TransportResult<ConnectedSyncMode<N>> {
        let provider =
            RootProvider::<N>::new(ClientBuilder::default().ws(WsConnect::new(ws_url)).await?);

        Ok(ConnectedSyncMode { _provider: provider })
    }

    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<ConnectedSyncMode<N>> {
        let provider = RootProvider::<N>::new(ClientBuilder::default().ipc(ipc_path.into()).await?);

        Ok(ConnectedSyncMode { _provider: provider })
    }

    pub fn connect<N: Network>(self, provider: RootProvider<N>) -> ConnectedSyncMode<N> {
        ConnectedSyncMode { _provider: provider }
    }
}

impl<N: Network> ConnectedSyncMode<N> {
    pub fn stream(self) -> EventStream {
        EventStream
    }
}

impl LatestMode {
    pub fn contract_address(mut self, address: Address) -> Self {
        self.address = address;
        self
    }

    pub fn event(mut self, signature: B256) -> Self {
        self.events.push(signature);
        self
    }

    pub fn events(mut self, signatures: Vec<B256>) -> Self {
        self.events.extend(signatures);
        self
    }

    pub fn max_reads(mut self, max: usize) -> Self {
        self.max_reads = max;
        self
    }

    pub async fn connect_ws<N: Network>(
        self,
        ws_url: Url,
    ) -> TransportResult<ConnectedLatestMode<N>> {
        let provider =
            RootProvider::<N>::new(ClientBuilder::default().ws(WsConnect::new(ws_url)).await?);

        Ok(ConnectedLatestMode { _provider: provider })
    }

    pub async fn connect_ipc<N: Network>(
        self,
        ipc_path: String,
    ) -> TransportResult<ConnectedLatestMode<N>> {
        let provider = RootProvider::<N>::new(ClientBuilder::default().ipc(ipc_path.into()).await?);

        Ok(ConnectedLatestMode { _provider: provider })
    }

    pub fn connect<N: Network>(self, provider: RootProvider<N>) -> ConnectedLatestMode<N> {
        ConnectedLatestMode { _provider: provider }
    }
}

impl<N: Network> ConnectedLatestMode<N> {
    pub fn stream(self) -> EventStream {
        EventStream
    }
}

#[cfg(test)]
mod demo {
    use alloy::network::Ethereum;

    use super::*;

    async fn demo_historic_earliest_to_latest() {
        let address = Address::ZERO;
        let event_sig = B256::ZERO;

        let scanner = EventScanner::historic()
            .contract_address(address)
            .event(event_sig)
            .connect_ws::<Ethereum>("wss://eth-mainnet.example.com".parse().unwrap())
            .await
            .unwrap();

        let _stream = scanner.stream();
    }

    // async fn demo_historic_from_3_to_latest() {
    //     let address = Address::ZERO;
    //     let event_sig = B256::ZERO;
    //
    //     let scanner = EventScanner::historic()
    //         .from_block(3u64)
    //         .contract_address(address)
    //         .event(event_sig)
    //         .connect_ws("wss://eth-mainnet.example.com".parse().unwrap())
    //         .await
    //         .unwrap();
    //
    //     let _stream = scanner.stream();
    // }
    //
    // async fn demo_historic_earliest_to_50() {
    //     let address = Address::ZERO;
    //     let event_sig = B256::ZERO;
    //
    //     let scanner = EventScanner::historic()
    //         .to_block(50u64)
    //         .contract_address(address)
    //         .event(event_sig)
    //         .connect_ws("wss://eth-mainnet.example.com".parse().unwrap())
    //         .await
    //         .unwrap();
    //
    //     let _stream = scanner.stream();
    // }
    //
    // async fn demo_historic_from_3_to_50() {
    //     let address = Address::ZERO;
    //     let event_sig = B256::ZERO;
    //
    //     let scanner = EventScanner::historic()
    //         .from_block(3u64)
    //         .to_block(50u64)
    //         .contract_address(address)
    //         .event(event_sig)
    //         .connect_ws("wss://eth-mainnet.example.com".parse().unwrap())
    //         .await
    //         .unwrap();
    //
    //     let _stream = scanner.stream();
    // }
    //
    // async fn demo_subscribe_mode() {
    //     let address = Address::ZERO;
    //     let event_sig = B256::ZERO;
    //
    //     let scanner = EventScanner::subscribe()
    //         .contract_address(address)
    //         .event(event_sig)
    //         .block_confirmations(4)
    //         .connect_ipc("/tmp/geth.ipc".to_string())
    //         .await
    //         .unwrap();
    //
    //     let _stream = scanner.stream();
    // }
    //
    // async fn demo_sync_mode() {
    //     let address = Address::ZERO;
    //     let event_sig = B256::ZERO;
    //
    //     let scanner = EventScanner::sync()
    //         .from_block(100u64)
    //         .contract_address(address)
    //         .event(event_sig)
    //         .block_confirmations(4)
    //         .connect_ws("wss://eth-mainnet.example.com".parse().unwrap())
    //         .await
    //         .unwrap();
    //
    //     let _stream = scanner.stream();
    // }
    //
    // async fn demo_latest_mode() {
    //     let address = Address::ZERO;
    //     let event_sig = B256::ZERO;
    //
    //     let scanner = EventScanner::latest(100)
    //         .contract_address(address)
    //         .event(event_sig)
    //         .connect_ws("wss://eth-mainnet.example.com".parse().unwrap())
    //         .await
    //         .unwrap();
    //
    //     let _stream = scanner.stream();
    // }
    //
    // async fn demo_multiple_events() {
    //     let address = Address::ZERO;
    //     let transfer_event = B256::ZERO;
    //     let approval_event = B256::ZERO;
    //
    //     let scanner = EventScanner::sync()
    //         .from_block(1000u64)
    //         .contract_address(address)
    //         .event(transfer_event)
    //         .event(approval_event)
    //         .block_confirmations(6)
    //         .connect_ws("wss://eth-mainnet.example.com".parse().unwrap())
    //         .await
    //         .unwrap();
    //
    //     let _stream = scanner.stream();
    // }
    //
    // async fn demo_events_vector() {
    //     let address = Address::ZERO;
    //     let transfer_event = B256::ZERO;
    //     let approval_event = B256::ZERO;
    //
    //     let scanner = EventScanner::sync()
    //         .from_block(1000u64)
    //         .contract_address(address)
    //         .events(vec![transfer_event, approval_event])
    //         .block_confirmations(6)
    //         .connect_ws("wss://eth-mainnet.example.com".parse().unwrap())
    //         .await
    //         .unwrap();
    //
    //     let _stream = scanner.stream();
    // }
}
