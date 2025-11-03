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

- `EventScanner` – the main scanner type the application will interact with. 
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
use alloy::{network::Ethereum, providers::{Provider, ProviderBuilder}, sol_types::SolEvent};
use event_scanner::{EventFilter, EventScannerBuilder, Message, robust_provider::RobustProvider};
use tokio_stream::StreamExt;

use crate::MyContract;

async fn run_scanner(
    ws_url: &str,
    contract: alloy::primitives::Address,
) -> Result<(), Box<dyn std::error::Error>> {
    // Connect to provider
    let provider = ProviderBuilder::new().connect(ws_url).await?;
    let robust_provider = RobustProvider::new(provider.root().to_owned());
    
    // Configure scanner with custom batch size (optional)
    let mut scanner = EventScannerBuilder::live()
        .max_block_range(500)  // Process up to 500 blocks per batch
        .connect(robust_provider);

    // Register an event listener
    let filter = EventFilter::new()
        .contract_address(contract)
        .event(MyContract::SomeEvent::SIGNATURE);

    let mut stream = scanner.subscribe(filter);

    // Start the scanner
    scanner.start().await?;

    // Process messages from the stream
    while let Some(message) = stream.next().await {
        match message {
            Message::Data(logs) => {
                println!("Received {} logs: {logs:?}", logs.len());
            }
            Message::Status(status) => {
                println!("Status update: {status:?}");
            }
            Message::Error(err) => {
                eprintln!("Error: {err}");
            }
        }
    }

    Ok(())
}
```

---

## Usage

### Building a Scanner

`EventScannerBuilder` provides mode-specific constructors and functions to configure settings before connecting.
Once configured, connect using:

- `connect(robust_provider)` - Connect using a `RobustProvider` wrapping your alloy provider

This will connect the `EventScanner` and allow you to create event streams and start scanning in various [modes](#scanning-modes).

```rust
use alloy::providers::{Provider, ProviderBuilder};
use event_scanner::robust_provider::RobustProvider;

// Connect to provider (example with WebSocket)
let provider = ProviderBuilder::new().connect("ws://localhost:8545").await?;
let robust_provider = RobustProvider::new(provider.root().to_owned());

// Live streaming mode
let scanner = EventScannerBuilder::live()
    .max_block_range(500)  // Optional: set max blocks per read (default: 1000)
    .block_confirmations(12)  // Optional: set block confirmations (default: 12)
    .connect(robust_provider.clone());

// Historical block range mode
let scanner = EventScannerBuilder::historic()
    .from_block(1_000_000)
    .to_block(2_000_000)
    .max_block_range(500)
    .connect(robust_provider.clone());

// Latest events mode
let scanner = EventScannerBuilder::latest(100)
    // .from_block(1_000_000)  // Optional: set start of search range
    // .to_block(2_000_000)    // Optional: set end of search range
    .max_block_range(500)
    .connect(robust_provider.clone());

// Sync from block then switch to live mode
let scanner = EventScannerBuilder::sync()
    .from_block(100)
    .max_block_range(500)
    .block_confirmations(12)
    .connect(robust_provider.clone());

// Sync the latest 60 events then switch to live mode
let scanner = EventScannerBuilder::sync()
    .from_latest(60)
    .block_confirmations(12)
    .connect(robust_provider);
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

### Event Filter Batch Builders

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

### Message Types

The scanner delivers three types of messages through the event stream:

- **`Message::Data(Vec<Log>)`** – Contains a batch of matching event logs. Each log includes the raw event data, transaction hash, block number, and other metadata.
- **`Message::Status(ScannerStatus)`** – Status notifications from the scanner:
- **`Message::Error(ScannerError)`** – Error notifications if the scanner encounters issues (e.g., RPC failures, connection problems)

Always handle all message types in your stream processing loop to ensure robust error handling and proper reorg detection.


### Scanning Modes

- **Live** – scanner that streams new blocks as they arrive. 
- **Historic** – scanner for streaming events from a past block range (default: genesis..=latest).
- **Latest Events** – scanner that collects up to `count` most recent events per listener. Final delivery is in chronological order (oldest to newest).
- **Sync from Block** – scanner that streams events from a given start block up to the current confirmed tip, then automatically transitions to live streaming.
- **Sync from Latest Events** - scanner that collects the most recent `count` events, then automatically transitions to live streaming.

#### Important Notes

- Set `max_block_range` based on your RPC provider's limits (e.g., Alchemy, Infura may limit queries to 2000 blocks). Default is 1000 blocks.
- The modes come with sensible defaults; for example, not specifying a start block for historic mode automatically sets it to the genesis block.
- For live mode, if the WebSocket subscription lags significantly (e.g., >2000 blocks), ranges are automatically capped to prevent RPC errors.

---

## Examples

- `examples/live_scanning` – minimal live-mode scanner using `EventScannerBuilder::live()`
- `examples/historical_scanning` – demonstrates replaying historical data using `EventScannerBuilder::historic()`
- `examples/sync_from_block_scanning` – demonstrates replaying from genesis (block 0) before continuing to stream the latest blocks using `EventScannerBuilder::sync().from_block(block_tag_or_number)`
- `examples/latest_events_scanning` – demonstrates scanning the latest events using `EventScannerBuilder::latest()`
- `examples/sync_from_latest_scanning` – demonstrates scanning the latest events before switching to live mode using `EventScannerBuilder::sync().from_latest(count)`.

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
