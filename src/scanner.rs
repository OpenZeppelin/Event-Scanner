use std::sync::Arc;

use alloy::{
    providers::{IpcConnect, Provider, ProviderBuilder, WsConnect},
    rpc::types::{Filter, Log},
};
use tokio::time::{Duration, sleep};
use tokio_stream::StreamExt;
use tracing::{error, info, warn};

use crate::{
    callback::EventCallback,
    types::{CallbackConfig, EventFilter},
};

enum ProviderType {
    WebSocket,
    Ipc,
}

pub struct Scanner {
    provider: Arc<dyn Provider>,
    current_head: Option<u64>,
    start_block: Option<u64>,
    max_blocks_per_filter: u64,
    tracked_events: Vec<EventFilter>,
    callback_config: CallbackConfig,
}

impl Scanner {
    pub async fn new(
        rpc_url: String,
        start_block: Option<u64>,
        max_blocks_per_filter: u64,
        tracked_events: Vec<EventFilter>,
        callback_config: CallbackConfig,
    ) -> anyhow::Result<Self> {
        let provider = match Self::detect_provider_type(&rpc_url) {
            Ok(ProviderType::WebSocket) => {
                info!("connecting to provider via WebSocket: {}", &rpc_url);
                let ws = WsConnect::new(&rpc_url);
                ProviderBuilder::new().connect_ws(ws).await?
            }
            Ok(ProviderType::Ipc) => {
                info!("connecting to provider via IPC: {}", &rpc_url);
                let ipc = IpcConnect::new(rpc_url.clone());
                ProviderBuilder::new().connect_ipc(ipc).await?
            }
            Err(e) => return Err(e),
        };

        Ok(Self {
            provider: Arc::new(provider),
            current_head: start_block,
            start_block,
            max_blocks_per_filter,
            tracked_events,
            callback_config,
        })
    }

    fn detect_provider_type(url: &str) -> anyhow::Result<ProviderType> {
        if url.starts_with("ws://") || url.starts_with("wss://") {
            Ok(ProviderType::WebSocket)
        } else if url.ends_with(".ipc") || url.contains("ipc") {
            Ok(ProviderType::Ipc)
        } else {
            Err(anyhow::anyhow!(
                "Unknown provider type for URL: {}. Expected ws://, wss://, or IPC path",
                url
            ))
        }
    }

    pub async fn start(&mut self) -> anyhow::Result<()> {
        match self.start_block {
            Some(start) => {
                self.process_historical_blocks(start).await?;
                // TODO: Ensure transition to live indexing is properly handled
                self.subscribe_to_new_blocks().await?
            }
            None => self.subscribe_to_new_blocks().await?,
        }
        Ok(())
    }

    pub fn current_head(&self) -> Option<u64> {
        self.current_head
    }

    pub fn add_event_filter(&mut self, filter: EventFilter) {
        self.tracked_events.push(filter);
    }

    async fn process_historical_blocks(&mut self, start_block: u64) -> anyhow::Result<()> {
        info!(start_block, "starting historical block processing");

        let mut current = start_block;
        let max_blocks = self.max_blocks_per_filter;

        loop {
            let chain_head = self.provider.get_block_number().await?;

            if current > chain_head {
                self.current_head = Some(current);
                info!(last_block = current, "Last block processed");
                break;
            }

            let to_block = (current + max_blocks).min(chain_head);

            info!(from_block = current, to_block, chain_head, "processing historical block range");

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
            let block_number = block.number;

            // TODO: Handle reorgs
            let from_block = match self.current_head {
                Some(head) => head,
                None => block_number,
            };
            let to_block = block_number;

            info!(from_block, to_block, "processing blocks:");

            self.process_block_events(from_block, to_block).await?;

            self.current_head = Some(block_number + 1);
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
                Ok(_) => return Ok(()),
                Err(e) => {
                    last_err = Some(e);
                    if attempt < attempts {
                        warn!(
                            attempt,
                            max_attempts = attempts,
                            "callback failed; retrying after fixed delay"
                        );
                        sleep(Duration::from_millis(config.delay_ms)).await;
                        continue;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("callback failed with unknown error")))
    }
}
