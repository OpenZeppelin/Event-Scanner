//! Binary to generate compressed Anvil state dumps with pre-generated events.
//!
//! This tool creates gzip-compressed Anvil blockchain state files containing a deployed
//! Counter contract with a specified number of events. These dump files can be loaded
//! by benchmarks for instant setup without regenerating events each time.
//!
//! The generated files are automatically compressed using gzip to reduce storage size.
//!
//! # Usage
//!
//! ```bash
//! cargo run --release --bin generate_dump -- --events 100000 --output benches/dumps/state_100000.json
//! ```
//!
//! Creates `state_100000.json.gz` (compressed dump) and `state_100000.metadata.json`.

// Allow precision loss when casting file sizes to f64 for display purposes
#![allow(clippy::cast_precision_loss)]

use alloy::{
    network::Ethereum,
    node_bindings::Anvil,
    primitives::Address,
    providers::{Provider, ProviderBuilder},
    sol,
};
use anyhow::{Context, Result};
use flate2::{Compression, write::GzEncoder};
use futures::future::try_join_all;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{BufReader, BufWriter},
    path::PathBuf,
    time::{Duration, Instant},
};

sol! {
    #[sol(rpc, bytecode="608080604052346015576101b0908161001a8239f35b5f80fdfe6080806040526004361015610012575f80fd5b5f3560e01c90816306661abd1461016157508063a87d942c14610145578063d732d955146100ad5763e8927fbc14610048575f80fd5b346100a9575f3660031901126100a9575f5460018101809111610095576020817f7ca2ca9527391044455246730762df008a6b47bbdb5d37a890ef78394535c040925f55604051908152a1005b634e487b7160e01b5f52601160045260245ffd5b5f80fd5b346100a9575f3660031901126100a9575f548014610100575f198101908111610095576020817f53a71f16f53e57416424d0d18ccbd98504d42a6f98fe47b09772d8f357c620ce925f55604051908152a1005b60405162461bcd60e51b815260206004820152601860248201527f436f756e742063616e6e6f74206265206e6567617469766500000000000000006044820152606490fd5b346100a9575f3660031901126100a95760205f54604051908152f35b346100a9575f3660031901126100a9576020905f548152f3fea2646970667358221220471585b420a1ad0093820ff10129ec863f6df4bec186546249391fbc3cdbaa7c64736f6c634300081e0033")]
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

/// Metadata for a generated dump file.
#[derive(Debug, Serialize, Deserialize)]
pub struct DumpMetadata {
    /// Number of events in the dump.
    pub event_count: usize,
    /// Contract address in the dump.
    pub contract_address: Address,
    /// Block number at time of dump.
    pub block_number: u64,
    /// Time taken to generate events.
    pub generation_time_secs: f64,
    /// Anvil version used (if available).
    pub anvil_version: Option<String>,
}

/// Command line arguments.
struct Args {
    /// Number of events to generate.
    events: usize,
    /// Output path for the dump file.
    output: PathBuf,
}

impl Args {
    fn parse() -> Result<Self> {
        let args: Vec<String> = std::env::args().collect();

        let mut events = None;
        let mut output = None;

        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--events" | "-e" => {
                    i += 1;
                    events = Some(
                        args.get(i)
                            .context("--events requires a value")?
                            .parse::<usize>()
                            .context("--events must be a positive integer")?,
                    );
                }
                "--output" | "-o" => {
                    i += 1;
                    output = Some(PathBuf::from(args.get(i).context("--output requires a value")?));
                }
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                arg => {
                    anyhow::bail!("Unknown argument: {arg}. Use --help for usage.");
                }
            }
            i += 1;
        }

        Ok(Self {
            events: events.context("--events is required")?,
            output: output.context("--output is required")?,
        })
    }
}

fn print_help() {
    eprintln!(
        r"Generate Anvil state dumps with pre-generated events.

USAGE:
    generate_dump --events <COUNT> --output <PATH>

OPTIONS:
    -e, --events <COUNT>    Number of events to generate
    -o, --output <PATH>     Output path for the dump file (e.g., benches/dumps/state_10000.json)
    -h, --help              Print this help message

EXAMPLES:
    generate_dump --events 10000 --output benches/dumps/state_10000.json
    generate_dump -e 50000 -o benches/dumps/state_50000.json
"
    );
}

/// Generates events by calling the contract's `increase` function.
async fn generate_events<P>(contract: &Counter::CounterInstance<P>, count: usize) -> Result<()>
where
    P: Provider<Ethereum> + Clone,
{
    const BATCH_SIZE: usize = 1000;
    let total_batches = count.div_ceil(BATCH_SIZE);

    for batch in 0..total_batches {
        let start = batch * BATCH_SIZE;
        let end = std::cmp::min(start + BATCH_SIZE, count);
        let batch_count = end - start;

        // Send batch of transactions
        let mut pending_txs = Vec::with_capacity(batch_count);
        for _ in 0..batch_count {
            pending_txs.push(contract.increase().send().await?);
        }

        // Wait for all transactions in batch to be confirmed
        let watch_futures = pending_txs.into_iter().map(|tx| async move { tx.watch().await });
        try_join_all(watch_futures).await?;

        let progress = ((batch + 1) * 100) / total_batches;
        eprintln!("  Progress: {progress}% ({end}/{count} events)");
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse()?;

    eprintln!("=== Anvil State Dump Generator ===\n");
    eprintln!("Events to generate: {}", args.events);
    eprintln!("Output path: {}\n", args.output.display());

    // Create output directory if it doesn't exist
    if let Some(parent) = args.output.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
    }

    // Spawn Anvil with dump-state option
    eprintln!("Starting Anvil...");
    let anvil = Anvil::new()
        .block_time_f64(0.01) // Fast block time for quick event generation
        .arg("--dump-state")
        .arg(&args.output)
        .try_spawn()
        .context("Failed to spawn Anvil. Is Foundry installed?")?;

    let anvil_version = get_anvil_version();
    eprintln!("Anvil started at: {}", anvil.endpoint());

    // Connect to Anvil
    let wallet = anvil.wallet().expect("Anvil should provide a default wallet");
    let provider = ProviderBuilder::new()
        .wallet(wallet)
        .connect(anvil.ws_endpoint_url().as_str())
        .await
        .context("Failed to connect to Anvil")?;

    // Deploy contract
    eprintln!("\nDeploying Counter contract...");
    let contract =
        Counter::deploy(provider.clone()).await.context("Failed to deploy Counter contract")?;
    let contract_address = *contract.address();
    eprintln!("Contract deployed at: {contract_address}");

    // Generate events
    eprintln!("\nGenerating {} events...", args.events);
    let start_time = Instant::now();
    generate_events(&contract, args.events).await?;
    let generation_time = start_time.elapsed();
    eprintln!("Events generated in {:.2}s", generation_time.as_secs_f64());

    // Get final block number
    let block_number = provider.get_block_number().await.context("Failed to get block number")?;

    // Create metadata
    let metadata = DumpMetadata {
        event_count: args.events,
        contract_address,
        block_number,
        generation_time_secs: generation_time.as_secs_f64(),
        anvil_version,
    };

    // Write metadata file
    let metadata_path = args.output.with_extension("metadata.json");
    let metadata_json =
        serde_json::to_string_pretty(&metadata).context("Failed to serialize metadata")?;
    fs::write(&metadata_path, &metadata_json)
        .with_context(|| format!("Failed to write metadata to {}", metadata_path.display()))?;

    eprintln!("\nMetadata written to: {}", metadata_path.display());

    // Drop Anvil to trigger state dump
    eprintln!("\nSaving Anvil state...");

    // Drop anvil - this triggers the dump
    drop(anvil);

    // Wait for the dump file to be written
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Verify dump file exists
    if !args.output.exists() {
        anyhow::bail!("Dump file was not created at {}", args.output.display());
    }

    let uncompressed_size = fs::metadata(&args.output)?.len();
    eprintln!(
        "Dump file created: {} ({:.2} MB)",
        args.output.display(),
        uncompressed_size as f64 / 1_000_000.0
    );

    // Compress the dump file
    eprintln!("\nCompressing dump file...");
    let compressed_path = compress_dump(&args.output)?;

    let compressed_size = fs::metadata(&compressed_path)?.len();
    let compression_ratio = (1.0 - (compressed_size as f64 / uncompressed_size as f64)) * 100.0;
    eprintln!(
        "Compressed: {} ({:.2} MB, {:.1}% reduction)",
        compressed_path.display(),
        compressed_size as f64 / 1_000_000.0,
        compression_ratio
    );

    // Remove uncompressed file
    fs::remove_file(&args.output).with_context(|| {
        format!("Failed to remove uncompressed file: {}", args.output.display())
    })?;

    eprintln!("\n=== Generation Complete ===");
    eprintln!("Compressed dump: {}", compressed_path.display());
    eprintln!("Metadata:        {}", metadata_path.display());
    eprintln!("\nThese files are ready to commit to the repository.");

    Ok(())
}

/// Compresses a dump file using gzip.
fn compress_dump(input_path: &PathBuf) -> Result<PathBuf> {
    let output_path = input_path.with_extension("json.gz");

    let input_file = File::open(input_path)
        .with_context(|| format!("Failed to open input file: {}", input_path.display()))?;
    let mut reader = BufReader::new(input_file);

    let output_file = File::create(&output_path)
        .with_context(|| format!("Failed to create output file: {}", output_path.display()))?;
    let writer = BufWriter::new(output_file);

    let mut encoder = GzEncoder::new(writer, Compression::default());
    std::io::copy(&mut reader, &mut encoder).context("Failed to compress dump file")?;

    encoder.finish().context("Failed to finalize compression")?;

    Ok(output_path)
}

/// Attempts to get the Anvil version.
fn get_anvil_version() -> Option<String> {
    std::process::Command::new("anvil")
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok().map(|s| s.trim().to_string()))
}
