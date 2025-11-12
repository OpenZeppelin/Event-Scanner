use alloy::{providers::ProviderBuilder, sol, sol_types::SolEvent};
use alloy_node_bindings::Anvil;
use event_scanner::{
    EventFilter, EventScannerBuilder, Message, robust_provider::builder::RobustProviderBuilder,
};
use tokio_stream::StreamExt;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

sol! {
    // Built directly with solc 0.8.30+commit.73712a01.Darwin.appleclang
    #[sol(rpc, bytecode="608080604052346015576101b0908161001a8239f35b5f80fdfe6080806040526004361015610012575f80fd5b5f3560e01c90816306661abd1461016157508063a87d942c14610145578063d732d955146100ad5763e8927fbc14610048575f80fd5b346100a9575f3660031901126100a9575f5460018101809111610095576020817f7ca2ca9527391044455246730762df008a6b47bbdb5d37a890ef78394535c040925f55604051908152a1005b634e487b7160e01b5f52601160045260245ffd5b5f80fd5b346100a9575f3660031901126100a9575f548015610100575f198101908111610095576020817f53a71f16f53e57416424d0d18ccbd98504d42a6f98fe47b09772d8f357c620ce925f55604051908152a1005b60405162461bcd60e51b815260206004820152601860248201527f436f756e742063616e6e6f74206265206e6567617469766500000000000000006044820152606490fd5b346100a9575f3660031901126100a95760205f54604051908152f35b346100a9575f3660031901126100a9576020905f548152f3fea2646970667358221220471585b420a1ad0093820ff10129ec863f6df4bec186546249391fbc3cdbaa7c64736f6c634300081e0033")]
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

    let anvil = Anvil::new().block_time_f64(0.5).try_spawn()?;
    let wallet = anvil.wallet();
    let provider = ProviderBuilder::new()
        .wallet(wallet.unwrap())
        .connect(anvil.ws_endpoint_url().as_str())
        .await?;
    let counter_contract = Counter::deploy(provider.clone()).await?;

    let contract_address = counter_contract.address();

    let increase_filter = EventFilter::new()
        .contract_address(*contract_address)
        .event(Counter::CountIncreased::SIGNATURE);

    let robust_provider = RobustProviderBuilder::new(provider)
        .max_timeout(std::time::Duration::from_secs(30))
        .max_retries(5)
        .min_delay(std::time::Duration::from_millis(500))
        .build()
        .await?;

    let mut scanner = EventScannerBuilder::latest(5).connect(robust_provider).await?;

    let mut stream = scanner.subscribe(increase_filter);

    for _ in 0..8 {
        _ = counter_contract.increase().send().await?;
    }

    scanner.start().await?;

    while let Some(message) = stream.next().await {
        match message {
            Message::Data(logs) => {
                for log in logs {
                    info!("Received event: {:?}", log.inner.data);
                }
            }
            Message::Error(e) => {
                error!("Received error: {}", e);
            }
            Message::Status(info) => {
                info!("Received status: {:?}", info);
            }
        }
    }

    Ok(())
}
