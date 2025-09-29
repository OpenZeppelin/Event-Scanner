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
use alloy::{eips::BlockNumberOrTag, network::Ethereum, sol_types::SolEvent};
use event_scanner::{EventFilter, event_scanner::EventScanner, block_range_scanner::Error};
use tokio_stream::StreamExt;

use crate::MyContract;

async fn run_scanner(
    ws_url: alloy::transports::http::reqwest::Url,
    contract: alloy::primitives::Address,
) -> Result<(), Error> {
    let mut client = EventScanner::new().connect_ws::<Ethereum>(ws_url).await?;

    let filter = EventFilter::new()
        .with_contract_address(contract)
        .with_event(MyContract::SomeEvent::SIGNATURE);

    let mut stream = client.create_event_stream(filter);

    tokio::spawn(async move {
        client.start_scanner(BlockNumberOrTag::Earliest, Some(BlockNumberOrTag::Latest)).await
    });

    while let Some(Ok(logs)) = stream.next().await {
        println!("Fetched logs: {logs:?}");
    }

    Ok(())
}
```

---

## Usage

### Building a Scanner

`EventScanner` supports:

- `with_blocks_read_per_epoch` - how many blocks are read at a time in a single batch (taken into consideration when fetching historical blocks)
- `with_reorg_rewind_depth` - how many blocks to rewind when a reorg is detected (NOTE ⚠️: still WIP)
- `with_block_confirmations` - how many confirmations to wait for before considering a block final (NOTE ⚠️: still WIP)

Once configured, connect using either `connect_ws::<Ethereum>(ws_url)` or `connect_ipc::<Ethereum>(path)`. This will `connect` the `EventScanner` and allow you to create event streams and start scanning in various [modes](#scanning-Modes).

### Defining Event Filters

Create an `EventFilter` for each contract/event pair you want to track. The filter specifies the contract address where events originated, and the event signature (from `SolEvent::SIGNATURE`).

Both `contract_address` and `event` fields are optional, allowing for flexible event tracking.

```rust
// Track a specific event from a specific contract
let specific_filter = EventFilter::new()
    .with_contract_address(*counter_contract.address())
    .with_event(Counter::CountIncreased::SIGNATURE);

// Track ALL events from a specific contract
let all_contract_events_filter = EventFilter::new()
    .with_contract_address(*counter_contract.address());

// Track ALL events from ALL contracts in the block range
let all_events_filter = EventFilter::new();
```

Register multiple filters by invoking `create_event_stream` repeatedly.

Event filters enable several powerful use cases:

- **Track all events from a specific contract**: Set `contract_address` but leave `event` as `None`
- **Track all events across all contracts**: Set both `contract_address` and `event` as `None`
- **Track specific events from specific contracts**: Set both fields (traditional usage)
- **Mixed filtering**: Use multiple filters with different optional field combinations

This flexibility allows you to build sophisticated event monitoring systems that can track events at different granularities depending on your application's needs.

### Scanning Modes

- **Live mode** – `start_scanner(BlockNumberOrTag::Latest, None)` subscribes to new blocks only.
- **Historical mode** – `start_scanner(BlockNumberOrTag::Number(start), Some(BlockNumberOrTag::Number(end)))`, scanner fetches events from a historical block range.
- **Historical → Live** – `start_scanner(BlockNumberOrTag::Number(start), None)` replays from `start` to current head, then streams future blocks.

For now modes are deduced from the `start` and `end` parameters. In the future, we might add explicit commands to select the mode.

See the integration tests under `tests/live_mode`, `tests/historic_mode`, and `tests/historic_to_live` for concrete examples.

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

