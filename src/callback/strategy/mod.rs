use std::sync::Arc;

use alloy::rpc::types::Log;
use async_trait::async_trait;

use crate::callback::EventCallback;

pub mod fixed_retry;
pub mod state_sync_aware;

pub use fixed_retry::{BACK_OFF_MAX_RETRIES, FixedRetryConfig, FixedRetryStrategy};
pub use state_sync_aware::{StateSyncAwareStrategy, StateSyncConfig};

#[async_trait]
pub trait CallbackStrategy: Send + Sync {
    async fn execute(
        &self,
        callback: &Arc<dyn EventCallback + Send + Sync>,
        log: &Log,
    ) -> anyhow::Result<()>;
}
