//! Benchmark utilities for Event Scanner performance testing.
//!
//! This module provides shared setup utilities for benchmarks, including
//! Anvil instance management, contract deployment, and event generation.
//!
//! ## Usage
//!
//! For benchmarks, use `setup_from_dump` to load pre-generated state:
//!
//! ```rust,ignore
//! mod helpers;
//! use std::path::Path;
//!
//! let env = helpers::setup_from_dump(Path::new("benches/dumps/state_100000.json.gz")).await?;
//! ```

use std::{
    fs::File,
    io::{BufReader, Read, Write},
    path::Path,
};

use alloy::{network::Ethereum, primitives::Address, providers::ProviderBuilder, sol};
use alloy_node_bindings::{Anvil, AnvilInstance};
use anyhow::Context;
use flate2::read::GzDecoder;
use robust_provider::{RobustProvider, RobustProviderBuilder};
use serde::{Deserialize, Serialize};

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

/// Pre-configured benchmark environment with an Anvil instance,
/// deployed contract, and generated events.
pub struct BenchEnvironment {
    /// The Anvil instance. Must be kept alive for the duration of the benchmark.
    #[allow(dead_code)]
    anvil: AnvilInstance,
    /// A robust provider connected to the Anvil instance.
    pub provider: RobustProvider<Ethereum>,
    /// The deployed contract address.
    pub contract_address: Address,
    /// The total number of events generated.
    pub event_count: usize,
    /// The latest block number at the time of dump.
    pub block_number: u64,
}

/// Metadata for a dump file, stored alongside the dump.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DumpMetadata {
    /// Number of events in the dump.
    pub event_count: usize,
    /// Contract address in the dump.
    pub contract_address: Address,
    /// Block number at time of dump.
    pub block_number: u64,
    /// Time taken to generate events (for reference).
    pub generation_time_secs: f64,
    /// Anvil version used to generate the dump.
    pub anvil_version: Option<String>,
}

/// Sets up a benchmark environment from a pre-generated dump file.
///
/// # Arguments
///
/// * `dump_path` - Path to the dump file. Can be `.json` or `.json.gz` (compressed).
///
/// # Errors
///
/// Returns an error if:
/// - The dump file cannot be read or decompressed
/// - The metadata file is missing or invalid
/// - Anvil fails to spawn or load the state
///
/// # Example
///
/// ```rust,ignore
/// let env = setup_from_dump(Path::new("benches/dumps/state_100000.json.gz")).await?;
/// ```
pub async fn setup_from_dump(dump_path: &Path) -> anyhow::Result<BenchEnvironment> {
    // Determine if compressed and get the JSON path
    let is_compressed = dump_path.extension().is_some_and(|ext| ext == "gz");

    let json_path = if is_compressed {
        // Decompress to a temporary location
        let decompressed_path = dump_path.with_extension("");
        decompress_dump(dump_path, &decompressed_path)?;
        decompressed_path
    } else {
        dump_path.to_path_buf()
    };

    // Load metadata
    let metadata_path = if is_compressed {
        // For .json.gz, metadata is at .metadata.json (strip .json.gz, add .metadata.json)
        let stem = dump_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let stem_without_json = stem.strip_suffix(".json").unwrap_or(stem);
        dump_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(format!("{stem_without_json}.metadata.json"))
    } else {
        dump_path.with_extension("metadata.json")
    };

    let metadata: DumpMetadata =
        serde_json::from_reader(File::open(&metadata_path).with_context(|| {
            format!("Failed to open metadata file: {}", metadata_path.display())
        })?)
        .with_context(|| format!("Failed to parse metadata file: {}", metadata_path.display()))?;

    // Start Anvil with the loaded state
    let anvil = Anvil::new()
        .arg("--load-state")
        .arg(&json_path)
        .try_spawn()
        .context("Failed to spawn Anvil with loaded state")?;

    // Connect to Anvil
    let provider = ProviderBuilder::new()
        .connect(anvil.ws_endpoint_url().as_str())
        .await
        .context("Failed to connect to Anvil")?;

    // Build robust provider for the scanner
    let robust_provider = RobustProviderBuilder::new(provider)
        .call_timeout(std::time::Duration::from_secs(30))
        .max_retries(5)
        .min_delay(std::time::Duration::from_millis(500))
        .build()
        .await?;

    Ok(BenchEnvironment {
        anvil,
        provider: robust_provider,
        contract_address: metadata.contract_address,
        event_count: metadata.event_count,
        block_number: metadata.block_number,
    })
}

/// Decompresses a gzip file to the specified output path.
fn decompress_dump(compressed_path: &Path, output_path: &Path) -> anyhow::Result<()> {
    let file = File::open(compressed_path).with_context(|| {
        format!("Failed to open compressed dump: {}", compressed_path.display())
    })?;

    let mut decoder = GzDecoder::new(BufReader::new(file));
    let mut decompressed = Vec::new();
    decoder
        .read_to_end(&mut decompressed)
        .with_context(|| format!("Failed to decompress dump: {}", compressed_path.display()))?;

    let mut output_file = File::create(output_path)
        .with_context(|| format!("Failed to create output file: {}", output_path.display()))?;
    output_file
        .write_all(&decompressed)
        .with_context(|| format!("Failed to write decompressed dump: {}", output_path.display()))?;

    Ok(())
}
