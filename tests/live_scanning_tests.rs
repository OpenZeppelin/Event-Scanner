use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use alloy::{providers::ProviderBuilder, rpc::types::Log, sol, sol_types::SolEvent};
use alloy_node_bindings::Anvil;
use async_trait::async_trait;
use event_scanner::{EventCallback, EventFilter, ScannerBuilder};
use tokio::time::sleep;

sol! {
    #[allow(missing_docs)]
    #[sol(rpc, bytecode="608080604052346015576101b0908161001a8239f35b5f80fdfe6080806040526004361015610012575f80fd5b5f3560e01c90816306661abd1461016157508063a87d942c14610145578063d732d955146100ad5763e8927fbc14610048575f80fd5b346100a9575f3660031901126100a9575f5460018101809111610095576020817f7ca2ca9527391044455246730762df008a6b47bbdb5d37a890ef78394535c040925f55604051908152a1005b634e487b7160e01b5f52601160045260245ffd5b5f80fd5b346100a9575f3660031901126100a9575f548015610100575f198101908111610095576020817f53a71f16f53e57416424d0d18ccbd98504d42a6f98fe47b09772d8f357c620ce925f55604051908152a1005b60405162461bcd60e51b815260206004820152601860248201527f436f756e742063616e6e6f74206265206e6567617469766500000000000000006044820152606490fd5b346100a9575f3660031901126100a95760205f54604051908152f35b346100a9575f3660031901126100a9576020905f548152f3fea2646970667358221220b846b706f79f5ae1fc4a4238319e723a092f47ce4051404186424739164ab02264736f6c634300081e0033")]
    contract LiveTestCounter {
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

struct EventCounter {
    pub count: Arc<AtomicUsize>,
}

#[async_trait]
impl EventCallback for EventCounter {
    async fn on_event(&self, _log: &Log) -> anyhow::Result<()> {
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct SlowProcessor {
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

#[tokio::test]
async fn test_live_scanning_basic() -> anyhow::Result<()> {
    let anvil = Anvil::new().block_time(1).try_spawn()?;
    let wallet = anvil.wallet();
    let provider =
        ProviderBuilder::new().wallet(wallet.unwrap()).connect(anvil.endpoint().as_str()).await?;

    let contract = LiveTestCounter::deploy(provider.clone()).await?;
    let contract_address = *contract.address();

    let event_count = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(EventCounter { count: event_count.clone() });

    let filter = EventFilter {
        contract_address,
        event: LiveTestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback,
    };

    let mut scanner =
        ScannerBuilder::new(anvil.ws_endpoint_url()).add_event_filter(filter).build().await?;

    let scanner_handle = tokio::spawn(async move { scanner.start().await });

    for _ in 0..5 {
        let _ = contract.increase().send().await?.get_receipt().await?;
    }

    sleep(Duration::from_millis(200)).await;

    scanner_handle.abort();

    assert_eq!(event_count.load(Ordering::SeqCst), 5);

    Ok(())
}

#[tokio::test]
async fn test_live_scanning_multiple_events() -> anyhow::Result<()> {
    let anvil = Anvil::new().block_time(1).try_spawn()?;
    let wallet = anvil.wallet();
    let provider =
        ProviderBuilder::new().wallet(wallet.unwrap()).connect(anvil.endpoint().as_str()).await?;

    let contract = LiveTestCounter::deploy(provider.clone()).await?;
    let contract_address = *contract.address();

    let increase_count = Arc::new(AtomicUsize::new(0));
    let decrease_count = Arc::new(AtomicUsize::new(0));

    let increase_callback = Arc::new(EventCounter { count: increase_count.clone() });

    let decrease_callback = Arc::new(EventCounter { count: decrease_count.clone() });

    let increase_filter = EventFilter {
        contract_address,
        event: LiveTestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback: increase_callback,
    };

    let decrease_filter = EventFilter {
        contract_address,
        event: LiveTestCounter::CountDecreased::SIGNATURE.to_owned(),
        callback: decrease_callback,
    };

    let mut scanner = ScannerBuilder::new(anvil.ws_endpoint_url())
        .add_event_filter(increase_filter)
        .add_event_filter(decrease_filter)
        .build()
        .await?;

    let scanner_handle = tokio::spawn(async move { scanner.start().await });

    for i in 0..6 {
        let _ = contract.increase().send().await?.get_receipt().await?;
        if i >= 4 {
            let _ = contract.decrease().send().await?.get_receipt().await?;
        }
    }

    sleep(Duration::from_millis(200)).await;

    scanner_handle.abort();

    assert_eq!(increase_count.load(Ordering::SeqCst), 6);
    assert_eq!(decrease_count.load(Ordering::SeqCst), 2);

    Ok(())
}

#[tokio::test]
async fn test_live_scanning_with_slow_processor() -> anyhow::Result<()> {
    let anvil = Anvil::new().block_time(1).try_spawn()?;
    let wallet = anvil.wallet();
    let provider =
        ProviderBuilder::new().wallet(wallet.unwrap()).connect(anvil.endpoint().as_str()).await?;

    let contract = LiveTestCounter::deploy(provider.clone()).await?;
    let contract_address = *contract.address();

    let processed = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(SlowProcessor { delay_ms: 100, processed: processed.clone() });

    let filter = EventFilter {
        contract_address,
        event: LiveTestCounter::CountIncreased::SIGNATURE.to_owned(),
        callback,
    };

    let mut scanner =
        ScannerBuilder::new(anvil.ws_endpoint_url()).add_event_filter(filter).build().await?;

    let scanner_handle = tokio::spawn(async move { scanner.start().await });

    for _ in 0..3 {
        let _ = contract.increase().send().await?.get_receipt().await?;
        // Less than processor delay
        sleep(Duration::from_millis(50)).await;
    }

    sleep(Duration::from_millis(200)).await;

    scanner_handle.abort();

    assert_eq!(processed.load(Ordering::SeqCst), 3);

    Ok(())
}
