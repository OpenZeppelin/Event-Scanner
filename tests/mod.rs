// This test module triggers the `test-utils` feature when running
// `cargo test`. Without it, you'd need to manually specify `--features test-utils`
// every time.
mod block_range_scanner;
mod common;
mod historic;
mod latest_events;
mod live;
mod sync;
