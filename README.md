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
  - [Scanning Latest Events Then Live](#scanning-latest-events-then-live)
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
use alloy::{eips::BlockNumberOrTag, network::Ethereum, sol_types::SolEvent};
use event_scanner::{EventFilter, EventScanner, EventScannerError, EventScannerMessage};
use tokio_stream::StreamExt;

use crate::MyContract;

async fn run_scanner(
    ws_url: alloy::transports::http::reqwest::Url,
    contract: alloy::primitives::Address,
) -> Result<(), EventScannerError> {
    let mut client = EventScanner::new().connect_ws::<Ethereum>(ws_url).await?;

    let filter = EventFilter::new()
        .with_contract_address(contract)
        .with_event(MyContract::SomeEvent::SIGNATURE);

    let mut stream = client.create_event_stream(filter);

    tokio::spawn(async move {
        client.start_scanner(BlockNumberOrTag::Earliest, Some(BlockNumberOrTag::Latest)).await
    });

    while let Some(EventScannerMessage::Data(logs)) = stream.next().await {
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
- `with_block_confirmations` - how many confirmations to wait for before considering a block final

Once configured, connect using one of:

- `connect_ws::<Ethereum>(ws_url)`
- `connect_ipc::<Ethereum>(path)`
- `connect_provider::<Ethereum>(provider)`

This will connect the `EventScanner` and allow you to create event streams and start scanning in various [modes](#scanning-modes).

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

Batch builder examples:

```rust
// Multiple contract addresses at once
let multi_addr = EventFilter::new()
    .with_contract_addresses([*counter_contract.address(), *other_counter_contract.address()]);

// Multiple event names at once
let multi_events = EventFilter::new()
    .with_events([Counter::CountIncreased::SIGNATURE, Counter::CountDecreased::SIGNATURE]);

// Multiple event signature hashes at once
let multi_sigs = EventFilter::new()
    .with_event_signatures([
        Counter::CountIncreased::SIGNATURE_HASH,
        Counter::CountDecreased::SIGNATURE_HASH,
    ]);
```

### Scanning Modes

- **Live mode** - `start_scanner(BlockNumberOrTag::Latest, None)` subscribes to new blocks only. On detecting a reorg, the scanner emits `ScannerStatus::ReorgDetected` and recalculates the confirmed window, streaming logs from the corrected confirmed block range.
- **Historical mode** - `start_scanner(BlockNumberOrTag::Number(start), Some(BlockNumberOrTag::Number(end)))`, scanner fetches events from a historical block range. Currently no reorg logic has been implemented (NOTE ⚠️: still WIP). In the case that the end block > finalized block and you need reorg resistance, we recommend you use sync mode.
- **Historical → Live** - `start_scanner(BlockNumberOrTag::Number(start), None)` replays from `start` to current head, then streams future blocks. Reorgs are handled as per the particular mode phase the scanner is in (historical or live).
- **Latest → Live** - `scan_latest_then_live(count)` fetches the most recent `count` events, then automatically transitions to live streaming. Ideal for applications that need recent historical context before monitoring real-time events.

For now modes are deduced from the `start` and `end` parameters. In the future, we might add explicit commands to select the mode.

See the integration tests under `tests/live_mode`, `tests/historic_mode`, `tests/historic_to_live`, and `tests/latest_events` for concrete examples.

### Scanning Latest Events

`scan_latest` collects the most recent matching events for each registered stream.

- It does not enter live mode; it scans a block range and then returns.
- Each registered stream receives at most `count` logs in a single message, chronologically ordered.

Basic usage:

```rust
use alloy::{eips::BlockNumberOrTag, network::Ethereum};
use event_scanner::{EventFilter, EventScanner, EventScannerMessage};
use tokio_stream::StreamExt;

async fn latest_example(ws_url: alloy::transports::http::reqwest::Url, addr: alloy::primitives::Address) -> eyre::Result<()> {
    let mut client = EventScanner::new().connect_ws::<Ethereum>(ws_url).await?;

    let filter = EventFilter::new().with_contract_address(addr);
    let mut stream = client.create_event_stream(filter);

    // Collect the latest 10 events across Earliest..=Latest
    client.scan_latest(10).await?;

    // Expect a single message with up to 10 logs, then the stream ends
    while let Some(msg) = stream.next().await {
        if let EventScannerMessage::Data(logs) = msg {
            println!("Latest logs: {}", logs.len());
        }
    }

    Ok(())
}
```

Restricting to a specific block range:

```rust
// Collect the latest 5 events between blocks [1_000_000, 1_100_000]
client
    .scan_latest_in_range(5, BlockNumberOrTag::Number(1_000_000), BlockNumberOrTag::Number(1_100_000))
    .await?;
```

The scanner periodically checks the tip to detect reorgs. On reorg, the scanner emits `ScannerStatus::ReorgDetected`, resets to the updated tip, and restarts the scan. Final delivery to log listeners is in chronological order.

Notes:

- Ensure you create streams via `create_event_stream()` before calling `scan_latest*` so listeners are registered.
- The function returns after delivering the messages; to continuously stream new blocks, use `scan_latest_then_live`.

### Scanning Latest Events Then Live

`scan_latest_then_live` combines the best of both worlds: it fetches recent historical events and then seamlessly transitions to live streaming mode for continuous monitoring.

**What it does:**

1. **Historical rewind phase**: Scans backwards from the current chain tip to collect up to `count` most recent matching events
2. **Automatic transition**: Emits `ScannerStatus::SwitchingToLive` to signal the mode change
3. **Live streaming phase**: Continuously monitors and streams new events as they arrive on-chain

**When to use it:**

This mode is ideal for applications that need recent historical context before monitoring real-time events, such as:
- Dashboards that show recent activity and update in real-time
- Analytics tools that need to "warm up" with recent data
- Event processors that need to catch up on missed events before going live

**How it works:**

The scanner captures the latest block number before starting to establish a clear boundary between phases. The historical phase scans from `Earliest` to `latest_block`, while the live phase starts from `latest_block + 1`. This design prevents duplicate events and handles race conditions where new blocks arrive during setup.

**Key behaviors:**

- **No duplicates**: Events are not delivered twice across the phase transition
- **Flexible count**: If fewer than `count` events exist, returns all available events
- **Reorg handling**: Both phases handle reorgs appropriately:
  - Historical phase: resets and rescans on reorg detection
  - Live phase: resets stream to the first post-reorg block that satisfies the block confirmations set via `with_block_confirmations`
- **Continuous operation**: Live phase continues indefinitely until the scanner is dropped

**Example:**

```rust
use alloy::{network::Ethereum, sol_types::SolEvent};
use event_scanner::{EventFilter, EventScanner, EventScannerMessage};
use tokio_stream::StreamExt;

async fn latest_then_live_example(
    ws_url: alloy::transports::http::reqwest::Url,
    addr: alloy::primitives::Address,
) -> eyre::Result<()> {
    let mut client = EventScanner::new().connect_ws::<Ethereum>(ws_url).await?;

    let filter = EventFilter::new().with_contract_address(addr);
    let mut stream = client.create_event_stream(filter);

    // Fetch the latest 10 events, then stream new events continuously
    client.scan_latest_then_live(10).await?;

    while let Some(msg) = stream.next().await {
        match msg {
            EventScannerMessage::Data(logs) => {
                println!("Received {} events", logs.len());
            }
            EventScannerMessage::Status(status) => {
                println!("Status update: {:?}", status);
                // You'll see ScannerStatus::SwitchingToLive when transitioning
            }
            EventScannerMessage::Error(e) => {
                eprintln!("Error: {}", e);
            }
        }
    }

    Ok(())
}
```

**Important notes:**

- Create event streams via `create_event_stream()` **before** calling `scan_latest_then_live`
- The method returns immediately; events are delivered asynchronously
- The live phase continues indefinitely until the scanner is dropped or encounters an error

---

## Examples

- `examples/simple_counter` – minimal live-mode scanner
- `examples/historical_scanning` – demonstrates replaying from genesis (block 0) before continuing streaming latest blocks
- `examples/latest_events_scanning` – demonstrates scanning the latest events using `scan_latest` (one-shot collection)
- `examples/latest_events_then_live_scanning` – demonstrates scanning the latest events using `scan_latest_then_live`.

Run an example with:

```bash
RUST_LOG=info cargo run -p simple_counter
```

All examples spin up a local `anvil` instance, deploy a demo counter contract, and demonstrate using event streams to process events.

---

## Testing

(We recommend using [nextest](https://crates.io/crates/cargo-nextest) to run the tests)

Integration tests cover all modes:

```bash
cargo nextest run
```

---

## Errors

The scanner surfaces errors via `EventScannerError`:

- `EventScannerError::BlockRangeScanner(BlockRangeScannerError)` – issues from the underlying block range service (e.g., subscription/channel constraints, historical/sync failures).
- `EventScannerError::Provider(RpcError<TransportErrorKind>)` – transport/provider-level failures (e.g., connection problems, RPC errors).

Status notifications are emitted as `EventScannerMessage::Status(ScannerStatus)`, including:

- `ScannerStatus::ReorgDetected`
- `ScannerStatus::SwitchingToLive`

