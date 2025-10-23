# Event Scanner

[![OpenSSF Scorecard](https://api.securityscorecards.dev/projects/github.com/OpenZeppelin/Event-Scanner/badge)](https://api.securityscorecards.dev/projects/github.com/OpenZeppelin/Event-Scanner)

> ⚠️ **WARNING: ACTIVE DEVELOPMENT** ⚠️
>
> This project is under active development and likely contains bugs. APIs and behaviour may change without notice. Use at your own risk.

## About

Event Scanner is a Rust library for streaming EVM-based smart contract events. It is built on top of the [`alloy`](https://github.com/alloy-rs/alloy) ecosystem and focuses on in-memory scanning without a backing database. Applications provide event filters; the scanner takes care of fetching historical ranges, bridging into live streaming mode, all whilst delivering the events as streams of data.

---

## Table of Contents

- [Features](#features)
- [Architecture Overview](#architecture-overview)
- [Quick Start](#quick-start)
- [Usage](#usage)
  - [Building a Scanner](#building-a-scanner)
  - [Defining Event Filters](#defining-event-filters)
  - [Scanning Modes](#scanning-modes)
  - [Scanning Latest Events](#scanning-latest-events)
- [Examples](#examples)
- [Testing](#testing)

---

## Features

- **Historical replay** – stream events from past block ranges.
- **Live subscriptions** – stay up to date with latest events via WebSocket or IPC transports.
- **Hybrid flow** – automatically transition from historical catch-up into streaming mode.
- **Latest events fetch** – one-shot rewind to collect the most recent matching logs.
- **Composable filters** – register one or many contract + event signature pairs.
- **No database** – processing happens in-memory; persistence is left to the host application.

---

## Architecture Overview

The library exposes two primary layers:

- `EventScanner` – the main module the application will interact with. 
- `BlockRangeScanner` – lower-level component that streams block ranges, handles reorg, batching, and provider subscriptions.

---

## Quick Start

Add `event-scanner` to your `Cargo.toml`:

```toml
[dependencies]
event-scanner = "0.3.0-alpha"
```

Create an event stream for the given event filters registered with the `EventScanner`:

```rust
use alloy::{network::Ethereum, sol_types::SolEvent};
use event_scanner::{EventFilter, EventScanner, EventScannerMessage};
use tokio_stream::StreamExt;

use crate::MyContract;

async fn run_scanner(
    ws_url: alloy::transports::http::reqwest::Url,
    contract: alloy::primitives::Address,
) -> Result<(), Box<dyn std::error::Error>> {
    // Configure scanner with custom batch size (optional)
    let mut scanner = EventScanner::live()
        .block_read_limit(500)  // Process up to 500 blocks per batch
        .connect_ws::<Ethereum>(ws_url).await?;

    let filter = EventFilter::new()
        .contract_address(contract)
        .event(MyContract::SomeEvent::SIGNATURE);

    let mut stream = scanner.subscribe(filter);

    // Start the scanner
    tokio::spawn(async move { scanner.stream().await });

    while let Some(EventScannerMessage::Data(logs)) = stream.next().await {
        println!("Fetched logs: {logs:?}");
    }

    Ok(())
}
```

---

## Usage

### Building a Scanner

`EventScanner` provides mode-specific constructors and a builder pattern to configure settings before connecting:

```rust
// Live streaming mode
let scanner = EventScanner::live()
    .block_read_limit(500)  // Optional: set max blocks per read (default: 1000)
    .connect_ws::<Ethereum>(ws_url).await?;

// Historical scanning mode
let scanner = EventScanner::historic()
    .block_read_limit(500)
    .connect_ws::<Ethereum>(ws_url).await?;

// Sync mode (historical + live)
let scanner = EventScanner::sync()
    .block_read_limit(500)
    .connect_ws::<Ethereum>(ws_url).await?;

// Latest mode (recent blocks only)
let scanner = EventScanner::latest()
    .count(100)
    .block_read_limit(500)
    .connect_ws::<Ethereum>(ws_url).await?;
```

**Available Modes:**
- `EventScanner::live()` – Streams new blocks as they arrive
- `EventScanner::historic()` – Processes historical block ranges
- `EventScanner::sync()` – Processes historical data then transitions to live streaming
- `EventScanner::latest()` – Processes a specific number of events then optionally switches to live scanning mode

**Global Configuration Options:**
- `block_read_limit(usize)` – Sets the maximum number of blocks to process per read operation. This prevents RPC provider errors from overly large block range queries.
- Connect with `connect_ws::<Ethereum>(url)`, `connect_ipc::<Ethereum>(path)`, or `connect(provider)`.

**Starting the Scanner:**
Invoking `scanner.start()` starts the scanner in the specified mode.

### Defining Event Filters

Create an `EventFilter` for each event stream you wish to process. The filter specifies the contract address where events originated, and event signatures (tip: you can use the value stored in `SolEvent::SIGNATURE`).

```rust
// Track a SPECIFIC event from a SPECIFIC contract
let specific_filter = EventFilter::new()
    .contract_address(*counter_contract.address())
    .event(Counter::CountIncreased::SIGNATURE);

// Track a multiple events from a SPECIFIC contract
let specific_filter = EventFilter::new()
    .contract_address(*counter_contract.address())
    .event(Counter::CountIncreased::SIGNATURE)
    .event(Counter::CountDecreased::SIGNATURE);

// Track a SPECIFIC event from a ALL contracts
let specific_filter = EventFilter::new()
    .event(Counter::CountIncreased::SIGNATURE);

// Track ALL events from a SPECIFIC contracts
let all_contract_events_filter = EventFilter::new()
    .contract_address(*counter_contract.address())
    .contract_address(*other_counter_contract.address());

// Track ALL events from ALL contracts in the block range
let all_events_filter = EventFilter::new();
```

Register multiple filters by invoking `subscribe` repeatedly.

The flexibility provided by `EventFilter` allows you to build sophisticated event monitoring systems that can track events at different granularities depending on your application's needs.

### Scanning Modes

- **Live mode** – `EventScanner::live()` creates a scanner that subscribes to new blocks as they arrive.
- **Historical mode** – `EventScanner::historic()` creates a scanner for processing historical block ranges.
- **Sync mode** – `EventScanner::sync()` creates a scanner that processes historical data then automatically transitions to live streaming.
- **Latest mode** – `EventScanner::latest()` creates a scanner that processes a set number of events.

**Configuration Tips:**
- Set `block_read_limit` based on your RPC provider's limits (e.g., Alchemy, Infura may limit queries to 2000 blocks)
- For live mode, if the WebSocket subscription lags significantly (e.g., >2000 blocks), ranges are automatically capped to prevent RPC errors
- Each mode has its own configuration options for start block, end block, confirmations, etc. where it makes sense
- The modes come with sensible defaults for example not specify a start block for historic mode automatically sets the start block to the earliest one

See integration tests under `tests/live_mode`, `tests/historic_mode`, and `tests/historic_to_live` for concrete examples.

### Scanning Latest Events

Scanner mode that collects a specified number of the most recent matching events for each registered stream.

- It does not enter live mode; it scans a block range and then returns.
- Each registered stream receives at most `count` logs in a single message, chronologically ordered.

Basic usage:

```rust
use alloy::{network::Ethereum, primitives::Address, transports::http::reqwest::Url};
use event_scanner::{EventFilter, EventScanner, Message};
use tokio_stream::StreamExt;

async fn latest_events(ws_url: Url, addr: Address) -> anyhow::Result<()> {
    let mut scanner = EventScanner::latest().count(10).connect_ws::<Ethereum>(ws_url).await?;

    let filter = EventFilter::new().contract_address(addr);

    let mut stream = scanner.subscribe(filter);

    // Collect the latest 10 events across Earliest..=Latest
    scanner.start().await?;

    // Expect a single message with up to 10 logs, then the stream ends
    while let Some(Message::Data(logs)) = stream.next().await {
        println!("Latest logs: {}", logs.len());
    }

    Ok(())
}
```

Restricting to a specific block range:

```rust
// Collect the latest 5 events between blocks [1_000_000, 1_100_000]
let mut scanner = EventScanner::latest()
    .count(5)
    .from_block(1_000_000)
    .to_block(1_100_000)
    .connect_ws::<Ethereum>(ws_url).await?;
    .await?;
```

The scanner periodically checks the tip to detect reorgs. On reorg, the scanner emits `ScannerStatus::ReorgDetected`, resets to the updated tip, and restarts the scan. Final delivery to log listeners is in chronological order.

Notes:

- Ensure you create streams via `subscribe()` before calling `start` so listeners are registered.
<!-- TODO: uncomment once implemented - The function returns after delivering the messages; to continuously stream new blocks, use `scan_latest_then_live`. -->

---

## Examples

- `examples/live_scanning` – minimal live-mode scanner using `EventScanner::live()`
- `examples/historical_scanning` – demonstrates replaying historical data using `EventScanner::historic()`
- `examples/latest_events_scanning` – demonstrates scanning the latest events using `EventScanner::latest()`

Run an example with:

```bash
RUST_LOG=info cargo run -p live_scanning    
# or
RUST_LOG=info cargo run -p historical_scanning
```

Both examples spin up a local `anvil` instance, deploy a demo counter contract, and demonstrate using event streams to process events.

---

## Testing

(We recommend using [nextest](https://crates.io/crates/cargo-nextest) to run the tests)

Integration tests cover all modes:

```bash
cargo nextest run
```

