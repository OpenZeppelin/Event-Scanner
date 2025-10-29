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
event-scanner = "0.4.0-alpha"
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

`EventScanner` provides mode-specific constructors and a builder pattern to configure settings before connecting.
Once configured, connect using one of:

- `connect_ws::<Ethereum>(ws_url)`
- `connect_ipc::<Ethereum>(path)`
- `connect::<Ethereum>(provider)`

This will connect the `EventScanner` and allow you to create event streams and start scanning in various [modes](#scanning-modes).

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

Batch builder examples:

```rust
// Multiple contract addresses at once
let multi_addr = EventFilter::new()
    .contract_addresses([*counter_contract.address(), *other_counter_contract.address()]);

// Multiple event names at once
let multi_events = EventFilter::new()
    .events([Counter::CountIncreased::SIGNATURE, Counter::CountDecreased::SIGNATURE]);

// Multiple event signature hashes at once
let multi_sigs = EventFilter::new()
    .event_signatures([
        Counter::CountIncreased::SIGNATURE_HASH,
        Counter::CountDecreased::SIGNATURE_HASH,
    ]);
```

### Scanning Modes

- **Live** – `EventScanner::live()` creates a scanner that streams new blocks as they arrive. On detecting a reorg, the scanner emits `ScannerStatus::ReorgDetected` and recalculates the confirmed window, streaming logs from the corrected confirmed block range.
- **Historic** – `EventScanner::historic()` creates a scanner for streaming events from a past block range. Currently no reorg logic has been implemented (NOTE ⚠️: still WIP).
- **Latest Events** – `EventScanner::latest()` creates a scanner that streams the specified number of recently emitted events. On detecting a reorg, the scanner re-fetches all of the events in the specified block range (default: Earliest..=Latest).
- **Sync from Block** – `EventScanner::sync().from_block(start)` creates a scanner that streams events from a given start block, and then automatically transitions to live streaming. Reorgs are handled as per the particular mode phase the scanner is in (historic or live).
- **Sync from Latest** - `EventScanner::sync().from_latest(count)` creates a scanner that streams the most recent `count` events, then automatically transitions to live streaming. Reorgs are handled as per the particular mode phase the scanner is in (latest events or live).

#### Configuration Tips

- Set `block_read_limit` based on your RPC provider's limits (e.g., Alchemy, Infura may limit queries to 2000 blocks)
- For live mode, if the WebSocket subscription lags significantly (e.g., >2000 blocks), ranges are automatically capped to prevent RPC errors
- Each mode has its own appropriate configuration options for start block, end block, confirmations
- The modes come with sensible defaults; for example not specifying a start block for historic mode automatically sets the start block to the genesis block.

See the integration tests under `tests/` for concrete examples.

---

## Examples

- `examples/live_scanning` – minimal live-mode scanner using `EventScanner::live()`
- `examples/historical_scanning` – demonstrates replaying historical data using `EventScanner::historic()`
- `examples/sync_from_block_scanning` – demonstrates replaying from genesis (block 0) before continuing to stream the latest blocks using `EventScanner::sync().from_block(block_tag_or_number)`
- `examples/latest_events_scanning` – demonstrates scanning the latest events using `EventScanner::latest()`
- `examples/sync_from_latest_scanning` – demonstrates scanning the latest events before switching to live mode using `EventScanner::sync().from_latest(count)`.

Run an example with:

```bash
RUST_LOG=info cargo run -p live_scanning
```

All examples spin up a local `anvil` instance, deploy a demo counter contract, and demonstrate using event streams to process events.

---

## Testing

(We recommend using [nextest](https://crates.io/crates/cargo-nextest) to run the tests)

Integration tests cover all modes:

```bash
cargo nextest run --features test-utils
```
