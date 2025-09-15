use std::sync::Arc;

use alloy::{
    providers::{IpcConnect, Provider, ProviderBuilder, WsConnect},
    rpc::types::{Filter, Log},
};
use tokio::time::{Duration, sleep};
use tokio_stream::StreamExt;
use tracing::{error, info, warn};

use crate::{callback::EventCallback, types::EventFilter};

enum ProviderType {
    WebSocket,
    Ipc,
}

pub struct Scanner {
    provider: Arc<dyn Provider>,
    current_head: Option<u64>,
    start_block: Option<u64>,
    end_block: Option<u64>,
    max_blocks_per_filter: u64,
    tracked_events: Vec<EventFilter>,
    callback_config: CallbackConfig,
}

impl Scanner {
    /// Creates a new scanner
    ///
    /// # Errors
    ///
    /// Returns an error if the scanner fails to start
    pub async fn new(
        rpc_url: String,
        start_block: Option<u64>,
        end_block: Option<u64>,
        max_blocks_per_filter: u64,
        tracked_events: Vec<EventFilter>,
        callback_config: CallbackConfig,
    ) -> anyhow::Result<Self> {
        let provider = Self::get_provider(&rpc_url).await?;

        Ok(Self {
            provider,
            current_head: start_block,
            start_block,
            end_block,
            max_blocks_per_filter,
            tracked_events,
            callback_config,
        })
    }

    async fn get_provider(url: &str) -> anyhow::Result<Arc<dyn Provider>> {
        let provider = match Self::detect_provider_type(url) {
            Ok(ProviderType::WebSocket) => {
                info!("connecting to provider via WebSocket: {}", url);
                let ws = WsConnect::new(url);
                ProviderBuilder::new().connect_ws(ws).await?
            }
            Ok(ProviderType::Ipc) => {
                info!("connecting to provider via IPC: {}", url);
                let ipc = IpcConnect::new(url.to_string());
                ProviderBuilder::new().connect_ipc(ipc).await?
            }
            Err(e) => return Err(e),
        };
        Ok(Arc::new(provider))
    }

    fn detect_provider_type(url: &str) -> anyhow::Result<ProviderType> {
        if url.starts_with("ws://") || url.starts_with("wss://") {
            Ok(ProviderType::WebSocket)
        } else if std::path::Path::new(url)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("ipc"))
            || url.contains("ipc")
        {
            Ok(ProviderType::Ipc)
        } else {
            Err(anyhow::anyhow!(
                "Unknown provider type for URL: {}. Expected ws://, wss://, or IPC path",
                url
            ))
        }
    }

    /// Starts the scanner
    ///
    /// # Errors
    ///
    /// Returns an error if the scanner fails to start
    pub async fn start(&mut self) -> anyhow::Result<()> {
        match (self.start_block, self.end_block) {
            (None, Some(end)) => {
                info!("Scanning from genesis block 0 to {}", end);
                self.process_historical_blocks(0, Some(end)).await?;
            }
            (Some(start), Some(end)) => {
                info!("Scanning from block {} to {}", start, end);
                self.process_historical_blocks(start, Some(end)).await?;
            }
            (Some(start), None) => {
                info!("Scanning from block {} to latest, then switching to live mode", start);
                self.process_historical_blocks(start, None).await?;
                self.subscribe_to_new_blocks().await?;
            }
            (None, None) => {
                info!("Starting in live mode only");
                self.subscribe_to_new_blocks().await?;
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn current_head(&self) -> Option<u64> {
        self.current_head
    }

    pub fn add_event_filter(&mut self, filter: EventFilter) {
        self.tracked_events.push(filter);
    }

    async fn process_historical_blocks(
        &mut self,
        start_block: u64,
        end_block: Option<u64>,
    ) -> anyhow::Result<()> {
        info!(start_block, end_block = ?end_block, "starting historical block processing");

        let mut current = start_block;
        let max_blocks = self.max_blocks_per_filter;

        let target_end = match end_block {
            Some(end) => end,
            None => self.provider.get_block_number().await?,
        };

        loop {
            if current > target_end {
                self.current_head = Some(target_end);
                info!(last_block = target_end, "Historical processing completed");
                break;
            }

            let to_block = (current + max_blocks - 1).min(target_end);

            info!(from_block = current, to_block, target_end, "processing historical block range");

            self.process_block_events(current, to_block).await?;

            current = to_block + 1;
        }

        Ok(())
    }

    async fn subscribe_to_new_blocks(&mut self) -> anyhow::Result<()> {
        info!("starting scanner in live mode");
        let sub = self.provider.subscribe_blocks().await?;
        let mut stream = sub.into_stream();

        while let Some(block) = stream.next().await {
            let latest_block_number = block.number;

            // TODO: Handle reorgs
            let from_block = match self.current_head {
                Some(head) => head,
                None => latest_block_number,
            };
            let to_block = latest_block_number;

            info!(from_block, to_block, "processing blocks:");

            self.process_block_events(from_block, to_block).await?;

            self.current_head = Some(latest_block_number + 1);
        }

        Ok(())
    }

    async fn process_block_events(&self, from_block: u64, to_block: u64) -> anyhow::Result<()> {
        for event_filter in &self.tracked_events {
            let filter = Filter::new()
                .address(event_filter.contract_address)
                .event(event_filter.event.as_str())
                .from_block(from_block)
                .to_block(to_block);

            match self.provider.get_logs(&filter).await {
                Ok(logs) => {
                    for log in &logs {
                        info!(
                            contract = ?event_filter.contract_address,
                            event = event_filter.event,
                            log_count = &logs.len(),
                            at_block = log.block_number,
                            from_block,
                            to_block,
                            "found logs for event in block range"
                        );
                        if let Err(e) = self.invoke_with_retry(&event_filter.callback, log).await {
                            error!(
                                contract = ?event_filter.contract_address,
                                event = event_filter.event,
                                error = %e,
                                "failed to invoke callback after retries"
                            );
                        }
                    }
                }
                Err(e) => {
                    error!(
                        contract = ?event_filter.contract_address,
                        event = event_filter.event,
                        error = %e,
                        from_block,
                        to_block,
                        "failed to get logs for block range"
                    );
                }
            }
        }

        Ok(())
    }

    async fn invoke_with_retry(
        &self,
        callback: &Arc<dyn EventCallback + Send + Sync>,
        log: &Log,
    ) -> anyhow::Result<()> {
        Self::invoke_with_retry_static(callback, log, &self.callback_config).await
    }

    async fn invoke_with_retry_static(
        callback: &Arc<dyn EventCallback + Send + Sync>,
        log: &Log,
        config: &CallbackConfig,
    ) -> anyhow::Result<()> {
        let attempts = config.max_attempts.max(1);
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 1..=attempts {
            match callback.on_event(log).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last_err = Some(e);
                    if attempt < attempts {
                        warn!(
                            attempt,
                            max_attempts = attempts,
                            "callback failed; retrying after fixed delay"
                        );
                        sleep(Duration::from_millis(config.delay_ms)).await;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("callback failed with unknown error")))
    }
}

#[cfg(test)]
mod tests {
    use alloy_node_bindings::Anvil;

    use crate::ScannerBuilder;

    use super::*;

    #[test]
    fn test_detect_provider_type_websocket() {
        assert!(matches!(
            Scanner::detect_provider_type("ws://localhost:8545"),
            Ok(ProviderType::WebSocket)
        ));
        assert!(matches!(
            Scanner::detect_provider_type("wss://mainnet.infura.io/ws"),
            Ok(ProviderType::WebSocket)
        ));
    }

    #[test]
    fn test_detect_provider_type_ipc() {
        assert!(matches!(Scanner::detect_provider_type("/tmp/geth.ipc"), Ok(ProviderType::Ipc)));
        assert!(matches!(
            Scanner::detect_provider_type("./path/to/node.ipc"),
            Ok(ProviderType::Ipc)
        ));
        assert!(matches!(
            Scanner::detect_provider_type("/var/run/geth/ipc"),
            Ok(ProviderType::Ipc)
        ));
    }

    #[test]
    fn test_detect_provider_type_invalid() {
        assert!(Scanner::detect_provider_type("http://localhost:8545").is_err());
        assert!(Scanner::detect_provider_type("https://mainnet.infura.io").is_err());
        assert!(Scanner::detect_provider_type("invalid-url").is_err());
    }

    #[tokio::test]
    async fn test_builds_ws_provider() {
        let anvil = Anvil::new().try_spawn().unwrap();
        let rpc_url = anvil.ws_endpoint_url();
        let scanner = ScannerBuilder::new(rpc_url).build().await;
        assert!(scanner.is_ok());
    }
}
