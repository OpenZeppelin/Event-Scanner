use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use alloy::{rpc::types::Log, sol_types::SolEvent};
use async_trait::async_trait;
use event_scanner::EventCallback;
use tokio::{sync::Mutex, time::sleep};

use crate::common::TestCounter;

pub struct BasicCounterCallback {
    pub count: Arc<AtomicUsize>,
}

#[async_trait]
impl EventCallback for BasicCounterCallback {
    async fn on_event(&self, _log: &Log) -> anyhow::Result<()> {
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

pub struct SlowProcessorCallback {
    pub delay_ms: u64,
    pub processed: Arc<AtomicUsize>,
}

#[async_trait]
impl EventCallback for SlowProcessorCallback {
    async fn on_event(&self, _log: &Log) -> anyhow::Result<()> {
        sleep(Duration::from_millis(self.delay_ms)).await;
        self.processed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// A callback that fails `max_fail_times` attempts before succeeding once.
pub struct FlakyCallback {
    pub attempts: Arc<AtomicUsize>,
    pub successes: Arc<AtomicUsize>,
    pub max_fail_times: usize,
}

#[async_trait]
impl EventCallback for FlakyCallback {
    async fn on_event(&self, _log: &Log) -> anyhow::Result<()> {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt <= self.max_fail_times {
            anyhow::bail!("intentional failure on attempt {attempt}");
        }
        self.successes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

// A callback that always fails and records attempts.
pub struct AlwaysFailingCallback {
    pub attempts: Arc<AtomicU64>,
}

#[async_trait]
impl EventCallback for AlwaysFailingCallback {
    async fn on_event(&self, _log: &Log) -> anyhow::Result<()> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("always failing callback")
    }
}

// Captures block numbers in the order they are processed.
pub struct BlockOrderingCallback {
    pub blocks: Arc<Mutex<Vec<u64>>>,
}

#[async_trait]
impl EventCallback for BlockOrderingCallback {
    async fn on_event(&self, log: &Log) -> anyhow::Result<()> {
        let mut guard = self.blocks.lock().await;
        if let Some(n) = log.block_number {
            guard.push(n);
        }
        Ok(())
    }
}

// Captures decoded CountIncreased `newCount` values to verify callback/event ordering.
pub struct EventOrderingCallback {
    pub counts: Arc<Mutex<Vec<u64>>>,
}

#[async_trait]
impl EventCallback for EventOrderingCallback {
    async fn on_event(&self, log: &Log) -> anyhow::Result<()> {
        if let Some(&TestCounter::CountIncreased::SIGNATURE_HASH) = log.topic0() {
            let TestCounter::CountIncreased { newCount } = log.log_decode()?.inner.data;
            let mut guard = self.counts.lock().await;
            guard.push(newCount.try_into().unwrap());
        }
        Ok(())
    }
}
