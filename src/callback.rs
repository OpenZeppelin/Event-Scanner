use alloy::rpc::types::Log;
use async_trait::async_trait;

#[async_trait]
pub trait EventCallback {
    /// Called when a matching log is found.
    async fn on_event(&self, log: &Log) -> anyhow::Result<()>;
}
