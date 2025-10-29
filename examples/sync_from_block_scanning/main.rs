use std::time::Duration;

use alloy::{network::Ethereum, providers::ProviderBuilder, sol, sol_types::SolEvent};
use alloy_node_bindings::Anvil;
use event_scanner::{EventFilter, EventScanner, Message};
use tokio::time::sleep;
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

    let anvil = Anvil::new().block_time(1).try_spawn()?;
    let wallet = anvil.wallet();
    let provider =
        ProviderBuilder::new().wallet(wallet.unwrap()).connect(anvil.endpoint().as_str()).await?;
    let counter_contract = Counter::deploy(provider).await?;

    let contract_address = counter_contract.address();

    let increase_filter = EventFilter::new()
        .contract_address(*contract_address)
        .event(Counter::CountIncreased::SIGNATURE);

    info!("Creating historical events...");
    for i in 0..3 {
        let _ = counter_contract.increase().send().await?.get_receipt().await?;
        info!("Historical event {} created", i + 1);
    }

    let mut scanner =
        EventScanner::sync().from_block(0).connect_ws::<Ethereum>(anvil.ws_endpoint_url()).await?;

    let mut stream = scanner.subscribe(increase_filter);

    info!("Starting sync scanner...");
    scanner.start().await.expect("failed to start scanner");

    info!("Creating live events...");
    for i in 0..2 {
        let _ = counter_contract.increase().send().await?.get_receipt().await?;
        info!("Live event {} created", i + 1);
        sleep(Duration::from_secs(1)).await;
    }

    let mut historical_processed = false;
    let mut live_processed = false;

    while let Some(message) = stream.next().await {
        match message {
            Message::Data(logs) => {
                for log in logs {
                    let Counter::CountIncreased { newCount } = log.log_decode().unwrap().inner.data;
                    if newCount <= 3 {
                        info!("Processed historical event: count = {}", newCount);
                        historical_processed = true;
                    } else {
                        info!("Processed live event: count = {}", newCount);
                        live_processed = true;
                    }
                }
            }
            Message::Error(e) => {
                error!("Received error: {}", e);
            }
            Message::Status(info) => {
                info!("Received status: {:?}", info);
            }
        }

        if historical_processed && live_processed {
            info!("Both historical and live events processed successfully!");
            break;
        }
    }

    Ok(())
}
