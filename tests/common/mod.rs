use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use alloy::{
    network::Ethereum, providers::ProviderBuilder, rpc::types::Log, sol, sol_types::SolEvent,
};
use alloy_node_bindings::{Anvil, AnvilInstance};
use async_trait::async_trait;
use event_scanner::EventCallback;
use tokio::sync::Mutex;
use tokio::time::sleep;

// Shared test contract used across integration tests
sol! {
    #[allow(missing_docs)]
    #[sol(rpc, bytecode="608080604052346015576101b0908161001a8239f35b5f80fdfe6080806040526004361015610012575f80fd5b5f3560e01c90816306661abd1461016157508063a87d942c14610145578063d732d955146100ad5763e8927fbc14610048575f80fd5b346100a9575f3660031901126100a9575f5460018101809111610095576020817f7ca2ca9527391044455246730762df008a6b47bbdb5d37a890ef78394535c040925f55604051908152a1005b634e487b7160e01b5f52601160045260245ffd5b5f80fd5b346100a9575f3660031901126100a9575f548015610100575f198101908111610095576020817f53a71f16f53e57416424d0d18ccbd98504d42a6f98fe47b09772d8f357c620ce925f55604051908152a1005b60405162461bcd60e51b815260206004820152601860248201527f436f756e742063616e6e6f74206265206e6567617469766500000000000000006044820152606490fd5b346100a9575f3660031901126100a95760205f54604051908152f35b346100a9575f3660031901126100a9576020905f548152f3fea2646970667358221220b846b706f79f5ae1fc4a4238319e723a092f47ce4051404186424739164ab02264736f6c634300081e0033")]
    contract TestCounter {
        uint256 public count;

        event CountIncreased(uint256 newCount);
        event CountDecreased(uint256 newCount);

        function increase() public {
            count += 1;
            emit CountIncreased(count);
        }

        function decrease() public {
            require(count > 0, "Count cannot be negative");
            count -= 1;
            emit CountDecreased(count);
        }

        function getCount() public view returns (uint256) {
            return count;
        }
    }
}

pub struct EventCounter {
    pub count: Arc<AtomicUsize>,
}

#[async_trait]
impl EventCallback for EventCounter {
    async fn on_event(&self, _log: &Log) -> anyhow::Result<()> {
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

pub struct SlowProcessor {
    pub delay_ms: u64,
    pub processed: Arc<AtomicUsize>,
}

#[async_trait]
impl EventCallback for SlowProcessor {
    async fn on_event(&self, _log: &Log) -> anyhow::Result<()> {
        sleep(Duration::from_millis(self.delay_ms)).await;
        self.processed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// A callback that fails `fail_times` attempts before succeeding once.
pub struct FlakyCallback {
    pub attempts: Arc<AtomicUsize>,
    pub successes: Arc<AtomicUsize>,
    pub fail_times: usize,
}

#[async_trait]
impl EventCallback for FlakyCallback {
    async fn on_event(&self, _log: &Log) -> anyhow::Result<()> {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt <= self.fail_times {
            anyhow::bail!("intentional failure on attempt {attempt}");
        }
        self.successes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

// A callback that always fails and records attempts.
pub struct AlwaysFailingCallback {
    pub attempts: Arc<AtomicUsize>,
}

#[async_trait]
impl EventCallback for AlwaysFailingCallback {
    async fn on_event(&self, _log: &Log) -> anyhow::Result<()> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("always failing callback")
    }
}

// Captures block numbers in the order they are processed.
pub struct OrderingCapture {
    pub blocks: Arc<Mutex<Vec<u64>>>,
}

#[async_trait]
impl EventCallback for OrderingCapture {
    async fn on_event(&self, log: &Log) -> anyhow::Result<()> {
        let mut guard = self.blocks.lock().await;
        if let Some(n) = log.block_number {
            guard.push(n);
        }
        Ok(())
    }
}

// Captures decoded CountIncreased `newCount` values to verify callback/event ordering.
pub struct OrderingCaptureCount {
    pub counts: Arc<Mutex<Vec<u64>>>,
}

#[async_trait]
impl EventCallback for OrderingCaptureCount {
    async fn on_event(&self, log: &Log) -> anyhow::Result<()> {
        if let Some(&TestCounter::CountIncreased::SIGNATURE_HASH) = log.topic0() {
            let TestCounter::CountIncreased { newCount } = log.log_decode()?.inner.data;
            let mut guard = self.counts.lock().await;
            guard.push(newCount.try_into().unwrap());
        }
        Ok(())
    }
}

#[allow(clippy::missing_errors_doc)]
pub fn spawn_anvil(block_time_secs: u64) -> anyhow::Result<AnvilInstance> {
    Ok(Anvil::new().block_time(block_time_secs).try_spawn()?)
}

#[allow(clippy::missing_errors_doc)]
#[allow(clippy::missing_panics_doc)]
pub async fn build_provider(
    anvil: &AnvilInstance,
) -> anyhow::Result<impl alloy::providers::Provider<Ethereum> + Clone> {
    let wallet = anvil.wallet().expect("anvil should return a default wallet");
    let provider = ProviderBuilder::new().wallet(wallet).connect(anvil.endpoint().as_str()).await?;
    Ok(provider)
}

#[allow(clippy::missing_errors_doc)]
pub async fn deploy_counter<P>(provider: P) -> anyhow::Result<TestCounter::TestCounterInstance<P>>
where
    P: alloy::providers::Provider<Ethereum> + Clone,
{
    let contract = TestCounter::deploy(provider).await?;
    Ok(contract)
}
