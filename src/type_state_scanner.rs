use alloy::{
    eips::BlockNumberOrTag,
    network::Network,
    providers::{RootProvider, WsConnect},
    rpc::client::ClientBuilder,
    transports::{TransportResult, http::reqwest::Url},
};

use crate::{
    block_range_scanner::{DEFAULT_BLOCK_CONFIRMATIONS, DEFAULT_BLOCKS_READ_PER_EPOCH},
    event_filter::EventFilter,
};

pub type Result<T> = std::result::Result<T, Error>;
pub type Error = Box<dyn std::error::Error + Send + Sync>;

pub struct EventStream;

#[derive(Clone, Debug)]
struct BaseConfig {
    event_filters: Vec<EventFilter>,
    max_reads: usize,
}

impl BaseConfig {
    fn new() -> Self {
        Self { event_filters: Vec::new(), max_reads: DEFAULT_BLOCKS_READ_PER_EPOCH }
    }
}

trait BaseConfigBuilder: Sized {
    fn base_mut(&mut self) -> &mut BaseConfig;

    fn event_filter(mut self, filter: EventFilter) -> Self {
        self.base_mut().event_filters.push(filter);
        self
    }

    fn event_filters(mut self, filters: Vec<EventFilter>) -> Self {
        self.base_mut().event_filters.extend(filters);
        self
    }

    fn max_reads(mut self, max: usize) -> Self {
        self.base_mut().max_reads = max;
        self
    }
}

pub struct HistoricMode {
    base: BaseConfig,
    from_block: BlockNumberOrTag,
    to_block: BlockNumberOrTag,
}

pub struct SubscribeMode {
    base: BaseConfig,
    block_confirmations: u64,
}

pub struct SyncMode {
    base: BaseConfig,
    from_block: BlockNumberOrTag,
    block_confirmations: u64,
}

pub struct LatestMode {
    base: BaseConfig,
    count: u64,
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
            base: BaseConfig::new(),
            from_block: BlockNumberOrTag::Earliest,
            to_block: BlockNumberOrTag::Latest,
        }
    }

    pub fn subscribe() -> SubscribeMode {
        SubscribeMode { base: BaseConfig::new(), block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS }
    }

    pub fn sync() -> SyncMode {
        SyncMode {
            base: BaseConfig::new(),
            from_block: BlockNumberOrTag::Earliest,
            block_confirmations: DEFAULT_BLOCK_CONFIRMATIONS,
        }
    }

    pub fn latest(count: u64) -> LatestMode {
        LatestMode { base: BaseConfig::new(), count }
    }
}

impl BaseConfigBuilder for HistoricMode {
    fn base_mut(&mut self) -> &mut BaseConfig {
        &mut self.base
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

impl BaseConfigBuilder for SubscribeMode {
    fn base_mut(&mut self) -> &mut BaseConfig {
        &mut self.base
    }
}

impl SubscribeMode {
    pub fn block_confirmations(mut self, count: u64) -> Self {
        self.block_confirmations = count;
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

impl BaseConfigBuilder for SyncMode {
    fn base_mut(&mut self) -> &mut BaseConfig {
        &mut self.base
    }
}

impl SyncMode {
    pub fn from_block(mut self, block: impl Into<BlockNumberOrTag>) -> Self {
        self.from_block = block.into();
        self
    }

    pub fn block_confirmations(mut self, count: u64) -> Self {
        self.block_confirmations = count;
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

impl BaseConfigBuilder for LatestMode {
    fn base_mut(&mut self) -> &mut BaseConfig {
        &mut self.base
    }
}

impl LatestMode {
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
    use alloy::{network::Ethereum, primitives::address};

    use super::*;

    async fn demo_historic_earliest_to_latest() {
        let contract_a = address!("0x000000000000000000000000000000000000dEaD");

        let filter = EventFilter::new()
            .with_contract_address(contract_a)
            .with_event("Transfer(address,address,uint256)");

        let scanner = EventScanner::historic()
            .event_filter(filter)
            .connect_ws::<Ethereum>("wss://eth-mainnet.example.com".parse().unwrap())
            .await
            .unwrap();

        let _stream = scanner.stream();
    }

    async fn demo_historic_from_3_to_latest() {
        let contract_a = address!("0x000000000000000000000000000000000000dEaD");

        let filter = EventFilter::new()
            .with_contract_address(contract_a)
            .with_event("Transfer(address,address,uint256)");

        let scanner = EventScanner::historic()
            .from_block(3u64)
            .event_filter(filter)
            .connect_ws::<Ethereum>("wss://eth-mainnet.example.com".parse().unwrap())
            .await
            .unwrap();

        let _stream = scanner.stream();
    }

    async fn demo_historic_earliest_to_50() {
        let contract_a = address!("0x000000000000000000000000000000000000dEaD");

        let filter = EventFilter::new()
            .with_contract_address(contract_a)
            .with_event("Transfer(address,address,uint256)");

        let scanner = EventScanner::historic()
            .to_block(50u64)
            .event_filter(filter)
            .connect_ws::<Ethereum>("wss://eth-mainnet.example.com".parse().unwrap())
            .await
            .unwrap();

        let _stream = scanner.stream();
    }

    async fn demo_historic_from_3_to_50() {
        let contract_a = address!("0x000000000000000000000000000000000000dEaD");

        let filter = EventFilter::new()
            .with_contract_address(contract_a)
            .with_event("Transfer(address,address,uint256)");

        let scanner = EventScanner::historic()
            .from_block(3u64)
            .to_block(50u64)
            .event_filter(filter)
            .connect_ws::<Ethereum>("wss://eth-mainnet.example.com".parse().unwrap())
            .await
            .unwrap();

        let _stream = scanner.stream();
    }

    async fn demo_subscribe_mode() {
        let contract_a = address!("0x000000000000000000000000000000000000dEaD");

        let filter = EventFilter::new()
            .with_contract_address(contract_a)
            .with_event("Transfer(address,address,uint256)");

        let scanner = EventScanner::subscribe()
            .event_filter(filter)
            .block_confirmations(4)
            .connect_ipc::<Ethereum>("/tmp/geth.ipc".to_string())
            .await
            .unwrap();

        let _stream = scanner.stream();
    }

    async fn demo_sync_mode() {
        let contract_a = address!("0x000000000000000000000000000000000000dEaD");

        let filter = EventFilter::new()
            .with_contract_address(contract_a)
            .with_event("Transfer(address,address,uint256)");

        let scanner = EventScanner::sync()
            .from_block(100u64)
            .event_filter(filter)
            .block_confirmations(4)
            .connect_ws::<Ethereum>("wss://eth-mainnet.example.com".parse().unwrap())
            .await
            .unwrap();

        let _stream = scanner.stream();
    }

    async fn demo_latest_mode() {
        let contract_a = address!("0x000000000000000000000000000000000000dEaD");

        let filter = EventFilter::new()
            .with_contract_address(contract_a)
            .with_event("Transfer(address,address,uint256)");

        let scanner = EventScanner::latest(100)
            .event_filter(filter)
            .connect_ws::<Ethereum>("wss://eth-mainnet.example.com".parse().unwrap())
            .await
            .unwrap();

        let _stream = scanner.stream();
    }

    async fn demo_multiple_event_filters() {
        let contract_a = address!("0x000000000000000000000000000000000000dEaD");
        let contract_b = address!("0x0000000000000000000000000000000000000001");

        let transfer = EventFilter::new()
            .with_contract_address(contract_a)
            .with_event("Transfer(address,address,uint256)");

        let approve = EventFilter::new()
            .with_contract_address(contract_b)
            .with_event("Approval(address,address,uint256)");

        let scanner = EventScanner::sync()
            .from_block(1000u64)
            .event_filter(transfer)
            .event_filter(approve)
            .block_confirmations(6)
            .connect_ws::<Ethereum>("wss://eth-mainnet.example.com".parse().unwrap())
            .await
            .unwrap();

        let _stream = scanner.stream();
    }

    async fn demo_event_filters_vector() {
        let contract_a = address!("0x000000000000000000000000000000000000dEaD");
        let contract_b = address!("0x0000000000000000000000000000000000000001");

        let transfer = EventFilter::new()
            .with_contract_address(contract_a)
            .with_event("Transfer(address,address,uint256)");

        let approve = EventFilter::new()
            .with_contract_address(contract_b)
            .with_event("Approval(address,address,uint256)");

        let scanner = EventScanner::sync()
            .from_block(1000u64)
            .event_filters(vec![transfer, approve])
            .block_confirmations(6)
            .connect_ws::<Ethereum>("wss://eth-mainnet.example.com".parse().unwrap())
            .await
            .unwrap();

        let _stream = scanner.stream();
    }
}
