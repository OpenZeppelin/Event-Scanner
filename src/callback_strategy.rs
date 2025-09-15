use std::{cmp, sync::Arc, time::Duration};

use alloy::rpc::types::Log;
use async_trait::async_trait;
use tracing::{info, warn};

use crate::callback::EventCallback;

// State sync aware retry settings
pub const BACK_OFF_MAX_RETRIES: u64 = 5;
pub const STATE_SYNC_RETRY_INTERVAL: Duration = Duration::from_secs(30);
pub const STATE_SYNC_MAX_RETRIES: u64 = 12;
pub const STATE_SYNC_RETRY_MAX_INTERVAL: Duration = Duration::from_secs(120);
pub const STATE_SYNC_RETRY_MAX_ELAPSED: Duration = Duration::from_secs(600);
pub const STATE_SYNC_RETRY_MULTIPLIER: f64 = 1.5; // exponential growth factor
pub const FIXED_DELAY_MS: u64 = 200;

#[async_trait]
pub trait CallbackStrategy: Send + Sync {
    async fn execute(
        &self,
        callback: &Arc<dyn EventCallback + Send + Sync>,
        log: &Log,
    ) -> anyhow::Result<()>;
}

#[derive(Clone, Copy, Debug)]
pub struct FixedRetryConfig {
    pub max_attempts: u64,
    pub delay_ms: u64,
}

impl Default for FixedRetryConfig {
    fn default() -> Self {
        Self { max_attempts: BACK_OFF_MAX_RETRIES, delay_ms: FIXED_DELAY_MS }
    }
}

pub struct FixedRetryStrategy {
    cfg: FixedRetryConfig,
}

impl FixedRetryStrategy {
    pub fn new(cfg: FixedRetryConfig) -> Self {
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

#[derive(Clone, Copy, Debug)]
pub struct StateSyncConfig {
    pub initial_interval: Duration,
    pub max_interval: Duration,
    pub max_elapsed: Duration,
    pub multiplier: f64,
}

impl Default for StateSyncConfig {
    fn default() -> Self {
        Self {
            initial_interval: STATE_SYNC_RETRY_INTERVAL,
            max_interval: STATE_SYNC_RETRY_MAX_INTERVAL,
            max_elapsed: STATE_SYNC_RETRY_MAX_ELAPSED,
            multiplier: STATE_SYNC_RETRY_MULTIPLIER,
        }
    }
}

pub struct StateSyncAwareStrategy {
    inner: FixedRetryStrategy,
    cfg: StateSyncConfig,
}

impl StateSyncAwareStrategy {
    pub fn new() -> Self {
        Self {
            inner: FixedRetryStrategy::new(FixedRetryConfig::default()),
            cfg: StateSyncConfig::default(),
        }
    }
    pub fn with_state_sync_config(mut self, cfg: StateSyncConfig) -> Self {
        self.cfg = cfg;
        self
    }

    pub fn with_fixed_retry_config(mut self, cfg: FixedRetryConfig) -> Self {
        self.inner = FixedRetryStrategy::new(cfg);
        self
    }
}

#[async_trait]
impl CallbackStrategy for StateSyncAwareStrategy {
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
                    let mut delay = self.cfg.initial_interval;
                    let start = tokio::time::Instant::now();
                    info!(initial_interval = ?self.cfg.initial_interval, max_interval = ?self.cfg.max_interval,
                        max_elapsed = ?self.cfg.max_elapsed, "Starting state-sync aware retry");
                    let mut last_err: anyhow::Error = first_err;
                    loop {
                        if start.elapsed() >= self.cfg.max_elapsed {
                            return Err(last_err);
                        }
                        tokio::time::sleep(delay).await;
                        match callback.on_event(log).await {
                            Ok(()) => return Ok(()),
                            Err(e) => {
                                last_err = e;
                                let next_secs = delay.as_secs_f64() * self.cfg.multiplier;
                                let next = Duration::from_secs_f64(next_secs);
                                delay = cmp::min(self.cfg.max_interval, next);
                                let elapsed = start.elapsed();
                                warn!(next_delay = ?delay, elapsed = ?elapsed, error = %last_err,
                                    "State-sync retry operation failed: will retry");
                            }
                        }
                    }
                } else {
                    // Fixed retry for regular errors
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
