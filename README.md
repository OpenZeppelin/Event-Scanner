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
event-scanner = "0.1.0-alpha.3"
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
    let mut client = EventScanner::new().connect_ws::<Ethereum>(ws_url).await?;

    let filter = EventFilter::new()
        .with_contract_address(contract)
        .with_event(MyContract::SomeEvent::SIGNATURE);

    let mut stream = client.create_event_stream(filter);

    // Live mode with default confirmations
    tokio::spawn(async move { client.stream_live(Option::None).await });

    while let Some(Ok(logs)) = stream.next().await {
        println!("Fetched logs: {logs:?}");
    }

    Ok(())
}
```

---

## Usage

### Building a Scanner

- No builders. Pass parameters to each mode as options.
- Defaults: `DEFAULT_BLOCKS_READ_PER_EPOCH`, `DEFAULT_BLOCK_CONFIRMATIONS`.
- Connect with `connect_ws::<Ethereum>(url)`, `connect_ipc::<Ethereum>(path)`, or an existing provider.

Mode APIs (all optional params use sensible defaults):
- Live: `client.stream_live(block_confirmations: Option<u64>)`
- Historical: `client.stream_historical(start, end, reads_per_epoch: Option<usize>)`
- From + Live: `client.stream_from(start, reads_per_epoch: Option<usize>, block_confirmations: Option<u64>)`

Pass `None` to use defaults; provide values to override per call.

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

// Track ALL events from a SPECIFIC contract
let all_contract_events_filter = EventFilter::new()
    .with_contract_address(*counter_contract.address());

// Track ALL events from ALL contracts in the block range
let all_events_filter = EventFilter::new();
```

Register multiple filters by invoking `create_event_stream` repeatedly.

The flexibility provided by `EventFilter` allows you to build sophisticated event monitoring systems that can track events at different granularities depending on your application's needs.

### Scanning Modes

- **Live mode** – `client.stream_live(None)` subscribes to new blocks using default confirmations.
- **Historical mode** – `client.stream_historical(start, end, None)` fetches a historical block range using the default batch size.
- **Historical → Live** – `client.stream_from(start, None, None)` replays from `start` to the confirmed tip, then streams future blocks with default confirmations.

Override defaults by passing `Some(...)` for the optional parameters.

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

