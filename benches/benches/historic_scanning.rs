//! Benchmarks for historic scanning mode.
//!
//! Heavy load tests that measure the time to fetch events from different block ranges.
//! Uses pre-generated Anvil state dumps for fast, reproducible setup.
//!
//! Benchmarks three block ranges from a 100k event dump:
//! - First 1/10 of blocks (~10k events)
//! - First 1/2 of blocks (~50k events)
//! - All blocks (100k events)

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::{Result, bail};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use event_scanner::{EventFilter, EventScannerBuilder, Message};
use event_scanner_benches::{BenchEnvironment, count_increased_signature, setup_from_dump};
use tokio_stream::StreamExt;

/// Returns the path to the dump file, resolved from the crate's manifest directory.
fn dump_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("dumps/state_100000.json.gz")
}

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

fn get_runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| tokio::runtime::Runtime::new().expect("failed to create tokio runtime"))
}

/// Runs a historic scan for a specific block range.
///
/// Fetches events from block 0 to `to_block`.
async fn run_historic_scan(env: &BenchEnvironment, to_block: u64) -> Result<()> {
    let filter = EventFilter::new()
        .contract_address(env.contract_address)
        .event(count_increased_signature());

    let mut scanner = EventScannerBuilder::historic()
        .max_block_range(100)
        .from_block(0)
        .to_block(to_block)
        .connect(env.provider.clone())
        .await?;

    let mut stream = scanner.subscribe(filter);
    scanner.start().await?;

    while let Some(message) = stream.next().await {
        match message {
            Ok(Message::Data(_)) => {}
            Ok(Message::Notification(notification)) => {
                bail!("Received unexpected notification: {notification:?}");
            }
            Err(e) => {
                bail!("Received error: {e}");
            }
        }
    }

    Ok(())
}

fn historic_scanning_benchmark(c: &mut Criterion) {
    let rt = get_runtime();

    let mut group = c.benchmark_group("historic_scanning");

    // Configure for heavy load tests (200s needed for slower CI runners)
    group.warm_up_time(std::time::Duration::from_secs(5));
    group.measurement_time(std::time::Duration::from_secs(200));

    // Load environment from pre-generated dump (100k events)
    println!("Loading benchmark environment from dump file...");
    let env: BenchEnvironment = rt.block_on(async {
        setup_from_dump(&dump_path()).await.expect("failed to load benchmark environment from dump")
    });
    println!(
        "Environment ready: {} events across {} blocks at contract {}",
        env.event_count, env.block_number, env.contract_address
    );

    // Calculate block ranges:
    // - 1/10 of blocks: ~10k events
    // - 1/2 of blocks: ~50k events
    // - All blocks: 100k events
    let total_blocks = env.block_number;
    let block_ranges = [
        (total_blocks / 10, "1/10 blocks (~10k events)"),
        (total_blocks / 2, "1/2 blocks (~50k events)"),
        (total_blocks, "all blocks (100k events)"),
    ];

    for (to_block, description) in block_ranges {
        println!("Benchmarking historic scan: {description} (to block {to_block})...");

        // Estimate events based on block ratio (events are roughly evenly distributed)
        let estimated_events = (env.event_count as u64 * to_block) / total_blocks;
        group.throughput(Throughput::Elements(estimated_events));

        group.bench_with_input(BenchmarkId::new("blocks", to_block), &to_block, |b, &to_block| {
            b.to_async(rt).iter(|| async {
                run_historic_scan(&env, to_block).await.expect("historic scan failed");
            });
        });
    }

    group.finish();
}

criterion_group!(benches, historic_scanning_benchmark);
criterion_main!(benches);
