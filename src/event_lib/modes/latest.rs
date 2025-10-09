// use alloy::{
//     network::Network,
//     providers::RootProvider,
//     transports::{TransportResult, http::reqwest::Url},
// };
//
// use tokio_stream::wrappers::ReceiverStream;
//
// use crate::event_lib::{
//     filter::EventFilter,
//     scanner::{ConnectedEventScanner, EventScannerError, EventScannerMessage},
// };
//
// use super::{BaseConfig, BaseConfigBuilder};
//
// pub struct LatestMode {
//     base: BaseConfig,
//     count: u64,
// }
//
// pub struct ConnectedLatestMode<N: Network> {
//     inner: ConnectedEventScanner<N>,
// }
//
// impl BaseConfigBuilder for LatestMode {
//     fn base_mut(&mut self) -> &mut BaseConfig {
//         &mut self.base
//     }
// }
//
// impl LatestMode {
//     pub(super) fn new() -> Self {
//         Self { base: BaseConfig::new(), count: 1 }
//     }
//
//     pub async fn connect_ws<N: Network>(
//         self,
//         ws_url: Url,
//     ) -> TransportResult<ConnectedLatestMode<N>> {
//         let brs = self.base.block_range_scanner.connect_ws::<N>(ws_url).await?;
//         Ok(ConnectedLatestMode { inner: Client::from_connected(brs) })
//     }
//
//     pub async fn connect_ipc<N: Network>(
//         self,
//         ipc_path: String,
//     ) -> TransportResult<ConnectedLatestMode<N>> {
//         let brs = self.base.block_range_scanner.connect_ipc::<N>(ipc_path).await?;
//         Ok(ConnectedLatestMode { inner: Client::from_connected(brs) })
//     }
//
//     pub fn connect<N: Network>(
//         self,
//         provider: RootProvider<N>,
//     ) -> TransportResult<ConnectedLatestMode<N>> {
//         let brs = self.base.block_range_scanner.connect_provider::<N>(provider)?;
//         Ok(ConnectedLatestMode { inner: Client::from_connected(brs) })
//     }
// }
//
// impl<N: Network> ConnectedLatestMode<N> {
//     pub fn create_event_stream(
//         &mut self,
//         filter: EventFilter,
//     ) -> ReceiverStream<EventScannerMessage> {
//         self.inner.create_event_stream(filter)
//     }
//
//     pub async fn stream(self) -> Result<(), EventScannerError> {
//         // For now, map Latest to live stream (count unused)
//         self.inner.stream_live(None).await
//     }
// }
