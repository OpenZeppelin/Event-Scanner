use std::{cmp, sync::Arc, time::Duration};

use alloy::rpc::types::Log;
use async_trait::async_trait;
use tracing::{info, warn};

use crate::{
    block_scanner::{
        STATE_SYNC_RETRY_INTERVAL, STATE_SYNC_RETRY_MAX_ELAPSED, STATE_SYNC_RETRY_MAX_INTERVAL,
        STATE_SYNC_RETRY_MULTIPLIER,
    },
    callback::EventCallback,
    types::CallbackConfig,
};

#[async_trait]
pub trait CallbackStrategy: Send + Sync {
    async fn execute(
        &self,
        callback: &Arc<dyn EventCallback + Send + Sync>,
        log: &Log,
    ) -> anyhow::Result<()>;
}

pub struct FixedRetryStrategy {
    cfg: CallbackConfig,
}

impl FixedRetryStrategy {
    pub fn new(cfg: CallbackConfig) -> Self {
        Self { cfg }
    }
}

#[async_trait]
impl CallbackStrategy for FixedRetryStrategy {
    async fn execute(
        &self,
        callback: &Arc<dyn EventCallback + Send + Sync>,
        log: &Log,
    ) -> anyhow::Result<()> {
        match callback.on_event(log).await {
            Ok(()) => Ok(()),
            Err(mut last_err) => {
                let attempts = self.cfg.max_attempts.max(1);
                for _ in 1..attempts {
                    warn!(
                        delay_ms = self.cfg.delay_ms,
                        max_attempts = attempts,
                        "Callback failed: retrying after fixed delay"
                    );
                    tokio::time::sleep(Duration::from_millis(self.cfg.delay_ms)).await;
                    match callback.on_event(log).await {
                        Ok(()) => return Ok(()),
                        Err(e) => last_err = e,
                    }
                }
                Err(last_err)
            }
        }
    }
}

pub struct StateSyncAwareStrategy<S: CallbackStrategy> {
    inner: S,
}

impl<S: CallbackStrategy> StateSyncAwareStrategy<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl<S: CallbackStrategy> CallbackStrategy for StateSyncAwareStrategy<S> {
    async fn execute(
        &self,
        callback: &Arc<dyn EventCallback + Send + Sync>,
        log: &Log,
    ) -> anyhow::Result<()> {
        match callback.on_event(log).await {
            Ok(()) => Ok(()),
            Err(first_err) => {
                if is_missing_trie_node_error(&first_err) {
                    // state sync aware retry path
                    let mut delay = STATE_SYNC_RETRY_INTERVAL;
                    let start = tokio::time::Instant::now();
                    info!(
                        initial_interval = ?STATE_SYNC_RETRY_INTERVAL,
                        max_interval = ?STATE_SYNC_RETRY_MAX_INTERVAL,
                        max_elapsed = ?STATE_SYNC_RETRY_MAX_ELAPSED,
                        "Starting state-sync aware retry"
                    );
                    let mut last_err: anyhow::Error = first_err;
                    loop {
                        if start.elapsed() >= STATE_SYNC_RETRY_MAX_ELAPSED {
                            return Err(last_err);
                        }
                        tokio::time::sleep(delay).await;
                        match callback.on_event(log).await {
                            Ok(()) => return Ok(()),
                            Err(e) => {
                                last_err = e;
                                let next_secs = delay.as_secs_f64() * STATE_SYNC_RETRY_MULTIPLIER;
                                let next = Duration::from_secs_f64(next_secs);
                                delay = cmp::min(STATE_SYNC_RETRY_MAX_INTERVAL, next);
                                let elapsed = start.elapsed();
                                warn!(next_delay = ?delay, elapsed = ?elapsed, error = %last_err,
                                    "State-sync retry operation failed: will retry");
                            }
                        }
                    }
                } else {
                    // delegate to inner strategy for regular errors
                    self.inner.execute(callback, log).await
                }
            }
        }
    }
}

fn is_missing_trie_node_error(err: &anyhow::Error) -> bool {
    let s = err.to_string().to_lowercase();
    s.contains("missing trie node") && s.contains("state") && s.contains("not available")
}

pub type DefaultCallbackStrategy = StateSyncAwareStrategy<FixedRetryStrategy>;
