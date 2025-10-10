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
  - [Working with Callbacks](#working-with-callbacks)
- [Examples](#examples)
- [Testing](#testing)

---

## Features

- **Historical replay** – stream events from past block ranges.
- **Live subscriptions** – stay up to date with latest events via WebSocket or IPC transports.
- **Hybrid flow** – automatically transition from historical catch-up into streaming mode.
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
event-scanner = "0.2.1-alpha"
```

Create an event stream for the given event filters registered with the `EventScanner`:

```rust
use alloy::{network::Ethereum, sol_types::SolEvent};
use event_scanner::{EventFilter, event_scanner::EventScanner};
use tokio_stream::StreamExt;

use crate::MyContract;

async fn run_scanner(
    ws_url: alloy::transports::http::reqwest::Url,
    contract: alloy::primitives::Address,
) -> Result<(), Box<dyn std::error::Error>> {
    // Configure scanner with custom batch size (optional)
    let mut client = EventScanner::new()
        .with_max_block_range(500)  // Process up to 500 blocks per batch
        .connect_ws::<Ethereum>(ws_url).await?;

    let filter = EventFilter::new()
        .with_contract_address(contract)
        .with_event(MyContract::SomeEvent::SIGNATURE);

    let mut stream = client.create_event_stream(filter);

    // Live mode with default confirmations
    tokio::spawn(async move { client.stream_live(None).await });

    while let Some(EventScannerMessage::Data(logs)) = stream.next().await {
        println!("Fetched logs: {logs:?}");
    }

    Ok(())
}
```

---

## Usage

### Building a Scanner

`EventScanner` provides a builder pattern to configure global settings before connecting:

```rust
let scanner = EventScanner::new()
    .with_max_block_range(500)  // Optional: set max blocks per batch (default: 1000)
    .connect_ws::<Ethereum>(ws_url).await?;
```

**Configuration Options:**
- `with_max_block_range(usize)` – Sets the maximum number of blocks to process per epoch. This applies to all modes (live, historical, and hybrid). Prevents RPC provider errors from overly large block range queries.
- Connect with `connect_ws::<Ethereum>(url)`, `connect_ipc::<Ethereum>(path)`, or `connect_provider(provider)`.

**Mode APIs** (all use the global `max_block_range` setting):
- Live: `client.stream_live(block_confirmations: Option<u64>)`
- Historical: `client.stream_historical(start, end)`
- From + Live: `client.stream_from(start, block_confirmations: Option<u64>)`

Pass `None` for block confirmations to use the default (`DEFAULT_BLOCK_CONFIRMATIONS = 0`).

### Defining Event Filters

Create an `EventFilter` for each event stream you wish to process. The filter specifies the contract address where events originated, and event signatures (tip: you can use the value stored in `SolEvent::SIGNATURE`).

```rust
// Track a SPECIFIC event from a SPECIFIC contract
let specific_filter = EventFilter::new()
    .with_contract_address(*counter_contract.address())
    .with_event(Counter::CountIncreased::SIGNATURE);

// Track a multiple events from a SPECIFIC contract
let specific_filter = EventFilter::new()
    .with_contract_address(*counter_contract.address())
    .with_event(Counter::CountIncreased::SIGNATURE)
    .with_event(Counter::CountDecreased::SIGNATURE);

// Track a SPECIFIC event from a ALL contracts
let specific_filter = EventFilter::new()
    .with_event(Counter::CountIncreased::SIGNATURE);

// Track ALL events from a SPECIFIC contracts
let all_contract_events_filter = EventFilter::new()
    .with_contract_address(*counter_contract.address())
    .with_contract_address(*other_counter_contract.address());

// Track ALL events from ALL contracts in the block range
let all_events_filter = EventFilter::new();
```

Register multiple filters by invoking `create_event_stream` repeatedly.

The flexibility provided by `EventFilter` allows you to build sophisticated event monitoring systems that can track events at different granularities depending on your application's needs.

### Scanning Modes

- **Live mode** – `client.stream_live(None)` subscribes to new blocks using default confirmations.
- **Historical mode** – `client.stream_historical(start, end)` fetches a historical block range using the configured `max_block_range`.
- **Historical → Live** – `client.stream_from(start, None)` replays from `start` to the confirmed tip, then streams future blocks.

**Configuration Tips:**
- Set `max_block_range` based on your RPC provider's limits (e.g., Alchemy, Infura may limit queries to 2000 blocks)
- For live mode, if the WebSocket subscription lags significantly (e.g., >2000 blocks), ranges are automatically capped to `max_block_range` to prevent RPC errors
- Pass `Some(block_confirmations)` to override the default confirmation depth (0) for added reorg protection

See integration tests under `tests/live_mode`, `tests/historic_mode`, and `tests/historic_to_live` for concrete examples.

---

## Examples

- `examples/simple_counter` – minimal live-mode scanner
- `examples/historical_scanning` – demonstrates replaying from genesis (block 0) before continuing streaming latest blocks

Run an example with:

```bash
RUST_LOG=info cargo run -p simple_counter
# or
RUST_LOG=info cargo run -p historical_scanning
```

Both examples spin up a local `anvil` instance, deploy a demo counter contract, and demonstrate using event streams to process events.

---

## Testing

Integration tests cover live, historical, and hybrid flows:
(We recommend using [nextest](https://crates.io/crates/cargo-nextest) to run the tests)

```bash
cargo nextest run
```

