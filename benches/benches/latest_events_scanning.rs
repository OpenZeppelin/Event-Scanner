//! Benchmarks for latest events scanning mode.
//!
//! Heavy load tests that measure the time to fetch the N most recent events
//! from a large pool of pre-generated events.
//! Uses pre-generated Anvil state dumps for fast, reproducible setup.
//!
//! Benchmarks fetching latest events from a 100k event pool:
//! - 10,000 latest events
//! - 50,000 latest events
//! - 100,000 latest events (all)

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::{Result, bail, ensure};
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

/// Runs a single latest events scan and asserts the expected event count.
///
/// This fetches the `latest_count` most recent events.
async fn run_latest_events_scan(env: &BenchEnvironment, latest_count: usize) -> Result<()> {
    let filter = EventFilter::new()
        .contract_address(env.contract_address)
        .event(count_increased_signature());

    let mut scanner =
        EventScannerBuilder::latest(latest_count).connect(env.provider.clone()).await?;

    let mut stream = scanner.subscribe(filter);
    scanner.start().await?;

    let mut log_count = 0;
    while let Some(message) = stream.next().await {
        match message {
            Ok(Message::Data(logs)) => {
                log_count += logs.len();
            }
            Ok(Message::Notification(notification)) => {
                bail!("Received unexpected notification: {notification:?}");
            }
            Err(e) => {
                bail!("Received error: {e}");
            }
        }
    }

    ensure!(log_count == latest_count, "expected {latest_count} events, got {log_count}");

    Ok(())
}

fn latest_events_scanning_benchmark(c: &mut Criterion) {
    let rt = get_runtime();

    let mut group = c.benchmark_group("latest_events_scanning");

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

    // Benchmark fetching latest N events from the 100k event pool:
    // - 10,000: Substantial history fetch
    // - 50,000: Heavy load retrieval
    // - 100,000: All events (full scan)
    for latest_count in [10_000, 50_000, 100_000] {
        println!("Benchmarking latest {latest_count} events...");

        group.throughput(Throughput::Elements(latest_count as u64));

        group.bench_with_input(
            BenchmarkId::new("latest", latest_count),
            &latest_count,
            |b, &count| {
                b.to_async(rt).iter(|| async {
                    run_latest_events_scan(&env, count).await.expect("latest events scan failed");
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, latest_events_scanning_benchmark);
criterion_main!(benches);
