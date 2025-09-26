# Event Scanner

[![OpenSSF Scorecard](https://api.securityscorecards.dev/projects/github.com/OpenZeppelin/event-scanner/badge)](https://api.securityscorecards.dev/projects/github.com/OpenZeppelin/event-scanner)

> ⚠️ **WARNING: ACTIVE DEVELOPMENT** ⚠️
>
> This project is under active development and likely contains bugs. APIs and behaviour may change without notice. Use at your own risk.

## About

Event Scanner is a Rust library for monitoring EVM-based smart contract events. It is built on top of the [`alloy`](https://github.com/alloy-rs/alloy) ecosystem and focuses on in-memory scanning without a backing database. Applications provide event filters and callback implementations; the scanner takes care of subscribing to historical ranges, bridging into live mode, and delivering events with retry-aware execution strategies.

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

- **Historical replay** – scan block ranges.
- **Live subscriptions** – stay up to date with latest blocks via WebSocket or IPC transports.
- **Hybrid flow** – automatically transition from historical catch-up into streaming mode.
- **Composable filters** – register one or many contract + event signature pairs with their own callbacks.
- **Retry strategies** – built-in retryable callback backoff strategies
- **No database** – processing happens in-memory; persistence is left to the host application.

---

## Architecture Overview

The library exposes two primary layers:

- `EventScannerBuilder` / `EventScanner` – the main module the application will interact with. 
- `BlockRangeScanner` – lower-level component that streams block ranges, handles reorg, batching, and provider subscriptions.

Callbacks implement the `EventCallback` trait. They are executed through a `CallbackStrategy` that performs retries when necessary before reporting failures.

---

## Quick Start

Add `event-scanner` to your `Cargo.toml`:

```toml
[dependencies]
event-scanner = "0.1.0-alpha.2"
```
Create a callback implementing `EventCallback` and register it with the builder:

```rust
use std::{sync::{Arc, atomic::{AtomicUsize, Ordering}}};
use alloy::{eips::BlockNumberOrTag, network::Ethereum, rpc::types::Log, sol_types::SolEvent};
use async_trait::async_trait;
use event_scanner::{event_scanner::EventScannerBuilder, EventCallback, EventFilter};

struct CounterCallback { processed: Arc<AtomicUsize> }

#[async_trait]
impl EventCallback for CounterCallback {
    async fn on_event(&self, _log: &Log) -> anyhow::Result<()> {
        self.processed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

async fn run_scanner(ws_url: alloy::transports::http::reqwest::Url, contract: alloy::primitives::Address) -> anyhow::Result<()> {
    let filter = EventFilter::new()
        .with_contract_address(contract)
        .with_event(MyContract::SomeEvent::SIGNATURE)
        .with_callback(Arc::new(CounterCallback { processed: Arc::new(AtomicUsize::new(0)) }));

    let mut client = EventScannerBuilder::new()
        .with_event_filter(filter)
        .connect_ws::<Ethereum>(ws_url)
        .await?;

    client.start_scanner(BlockNumberOrTag::Latest, None).await?;
    Ok(())
}
```

---

## Usage

### Building a Scanner

`EventScannerBuilder` supports:

- `with_event_filter(s)` – attach [filters](#defining-event-filters).
- `with_callback_strategy(strategy)` – override retry behaviour (`StateSyncAwareStrategy` by default).
- `with_blocks_read_per_epoch` - how many blocks are read at a time in a single batch (taken into consideration when fetching historical blocks)
- `with_reorg_rewind_depth` - how many blocks to rewind when a reorg is detected
- `with_block_confirmations` - how many confirmations to wait for before considering a block final

Once configured, connect using either `connect_ws::<Ethereum>(ws_url)` or `connect_ipc::<Ethereum>(path)`. This will `build` the `EventScanner` and allow you to call run to start in various [modes](#scanning-Modes).


### Defining Event Filters

Create an `EventFilter` for each contract/event pair you want to track. The filter bundles the contract address, the event signature (from `SolEvent::SIGNATURE`), and an `Arc<dyn EventCallback + Send + Sync>`.

Both `contract_address` and `event` fields are optional, allowing for flexible event tracking.

You can construct EventFilters using either the builder pattern (recommended) or direct struct construction:

### Builder Pattern (Recommended)

```rust
// Track a specific event from a specific contract
let specific_filter = EventFilter::new()
    .with_contract_address(*counter_contract.address())
    .with_event(Counter::CountIncreased::SIGNATURE)
    .with_callback(Arc::new(CounterCallback));

// Track ALL events from a specific contract
let all_contract_events_filter = EventFilter::new()
    .with_contract_address(*counter_contract.address())
    .with_callback(Arc::new(AllEventsCallback));

// Track ALL events from ALL contracts in the block range
let all_events_filter = EventFilter::new()
    .with_callback(Arc::new(GlobalEventsCallback));
```

### Direct Struct Construction

```rust
// Track a specific event from a specific contract (traditional usage)
let specific_filter = EventFilter {
    contract_address: Some(*counter_contract.address()),
    event: Some(Counter::CountIncreased::SIGNATURE.to_owned()),
    callback: Arc::new(CounterCallback),
};

// Track ALL events from a specific contract
let all_contract_events_filter = EventFilter {
    contract_address: Some(*counter_contract.address()),
    event: None, // Will track all events from this contract
    callback: Arc::new(AllEventsCallback),
};

// Track ALL events from ALL contracts in the block range
let all_events_filter = EventFilter {
    contract_address: None, // Will track events from all contracts
    event: None,            // Will track all event types
    callback: Arc::new(GlobalEventsCallback),
};
```

Register multiple filters by calling either `with_event_filter` repeatedly or `with_event_filters` once.

#### Use Cases for Optional Fields

The optional `contract_address` and `event` fields enable several powerful use cases:

- **Track all events from a specific contract**: Set `contract_address` but leave `event` as `None`
- **Track all events across all contracts**: Set both `contract_address` and `event` as `None`
- **Track specific events from specific contracts**: Set both fields (traditional usage)
- **Mixed filtering**: Use multiple filters with different optional field combinations

This flexibility allows you to build sophisticated event monitoring systems that can track events at different granularities depending on your application's needs.


### Scanning Modes

- **Live mode** – `start(BlockNumberOrTag::Latest, None)` subscribes to new blocks only.
- **Historical mode** – `start(BlockNumberOrTag::Number(start, Some(BlockNumberOrTag::Number(end)))`, scanner fetches events from a historical block range.
- **Historical → Live** – `start(BlockNumberOrTag::Number(start, None)` replays from `start` to current head, then streams future blocks.

For now modes are deduced from the `start` and `end` parameters. In the future, we might add explicit commands to select the mode.

See the integration tests under `tests/live_mode`, `tests/historic_mode`, and `tests/historic_to_live` for concrete examples.

### Working with Callbacks

Implement `EventCallback`:

```rust
#[async_trait]
impl EventCallback for RollupCallback {
    async fn on_event(&self, log: &Log) -> anyhow::Result<()> {
        // decode event, send to EL etc.
        Ok(())
    }
}
```

Advanced users can write custom retry behaviour by implementing the `CallbackStrategy` trait. The default `StateSyncAwareStrategy` automatically detects state-sync errors and performs exponential backoff ([smart retry mechanism](https://github.com/taikoxyz/taiko-mono/blob/f4b3a0e830e42e2fee54829326389709dd422098/packages/taiko-client/pkg/chain_iterator/block_batch_iterator.go#L149) from the geth driver) before falling back to a fixed retry policy configured via `FixedRetryConfig`.

```rust
#[async_trait]
pub trait CallbackStrategy: Send + Sync {
    async fn execute(
        &self,
        callback: &Arc<dyn EventCallback + Send + Sync>,
        log: &Log,
    ) -> anyhow::Result<()>;
}
```

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

Both examples spin up a local `anvil` instance and deploy a demo counter contract before starting the scanner.

---

## Testing

Integration tests cover live, historical, and hybrid flows:
(We recommend using [nextest](https://crates.io/crates/cargo-nextest) to run the tests)

```bash
cargo nextest run
```

