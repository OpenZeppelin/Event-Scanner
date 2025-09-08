use std::{sync::Arc, thread::spawn, time::Duration};

use alloy::providers::ProviderBuilder;
use alloy::rpc::types::Log;
use alloy::sol;
use alloy::sol_types::SolEvent;
use alloy_node_bindings::Anvil;
use async_trait::async_trait;
use event_scanner::{CallbackConfig, EventCallback, EventFilter, ScannerBuilder};

use tokio::time::sleep;
use tracing::info;
use tracing_subscriber::EnvFilter;

struct CounterCallback;

#[async_trait]
impl EventCallback for CounterCallback {
    async fn on_event(&self, log: &Log) -> anyhow::Result<()> {
        info!("Callback successfully executed with event {:?}", log.inner.data);
        Ok(())
    }
}

sol! {
    #[allow(missing_docs)]
    #[sol(rpc, bytecode="608080604052346015576101b0908161001a8239f35b5f80fdfe6080806040526004361015610012575f80fd5b5f3560e01c90816306661abd1461016157508063a87d942c14610145578063d732d955146100ad5763e8927fbc14610048575f80fd5b346100a9575f3660031901126100a9575f5460018101809111610095576020817f7ca2ca9527391044455246730762df008a6b47bbdb5d37a890ef78394535c040925f55604051908152a1005b634e487b7160e01b5f52601160045260245ffd5b5f80fd5b346100a9575f3660031901126100a9575f548015610100575f198101908111610095576020817f53a71f16f53e57416424d0d18ccbd98504d42a6f98fe47b09772d8f357c620ce925f55604051908152a1005b60405162461bcd60e51b815260206004820152601860248201527f436f756e742063616e6e6f74206265206e6567617469766500000000000000006044820152606490fd5b346100a9575f3660031901126100a95760205f54604051908152f35b346100a9575f3660031901126100a9576020905f548152f3fea2646970667358221220b846b706f79f5ae1fc4a4238319e723a092f47ce4051404186424739164ab02264736f6c634300081e0033")]
contract Counter {
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
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env()).try_init();

    let anvil = Anvil::new().block_time(1).try_spawn()?;
    let wallet = anvil.wallet();
    let provider =
        ProviderBuilder::new().wallet(wallet.unwrap()).connect(anvil.endpoint().as_str()).await?;
    let counter_contract = Counter::deploy(provider).await?;

    let contract_address = counter_contract.address();

    let increase_filter = EventFilter {
        contract_address: *contract_address,
        event: Counter::CountIncreased::SIGNATURE.to_owned(),
        callback: Arc::new(CounterCallback),
    };

    let mut scanner = ScannerBuilder::new(anvil.ws_endpoint_url())
        .add_event_filter(increase_filter)
        .callback_config(CallbackConfig { max_attempts: 3, delay_ms: 200 })
        .build()
        .await?;

    let task_1 = tokio::spawn(async move {
        scanner.start().await.expect("failed to start scanner");
    });

    let task_2 = tokio::spawn(async move {
        sleep(Duration::from_secs(5)).await;
        match counter_contract.increase().send().await {
            Ok(_) => info!("Tx sent succsefully"),
            Err(e) => {
                tracing::error!("Failed to send tx {:?}", e);
            }
        }
    });

    task_1.await.ok();
    task_2.await.ok();

    Ok(())
}
