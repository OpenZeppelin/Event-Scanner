use std::{sync::Arc, time::Duration};

use alloy::rpc::types::Log;
use async_trait::async_trait;
use tracing::warn;

use crate::callback::EventCallback;

use super::CallbackStrategy;

pub const BACK_OFF_MAX_RETRIES: u64 = 5;
pub const BACK_OFF_MAX_DELAY_MS: u64 = 200;

#[derive(Clone, Copy, Debug)]
pub struct FixedRetryConfig {
    pub max_attempts: u64,
    pub delay_ms: u64,
}

impl Default for FixedRetryConfig {
    fn default() -> Self {
        Self { max_attempts: BACK_OFF_MAX_RETRIES, delay_ms: BACK_OFF_MAX_DELAY_MS }
    }
}

pub struct FixedRetryStrategy {
    cfg: FixedRetryConfig,
}

impl FixedRetryStrategy {
    #[must_use]
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
