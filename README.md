# Event Scanner

> ⚠️ **WARNING: ACTIVE DEVELOPMENT** ⚠️
> 
> This project is under active development and likely contains many bugs. Use at your own risk!
> APIs and functionality may change without notice.

## About

Event Scanner is a Rust-based Ethereum blockchain event monitoring library built on top of the Alloy framework. It provides a flexible and efficient way to:

- Subscribe to and monitor specific smart contract events in real-time
- Process historical events from a specified starting block
- Handle event callbacks with configurable retry logic
- Support both WebSocket and IPC connections to Ethereum nodes
- Batch process events for improved performance

The scanner allows you to define custom event filters with associated callbacks, making it easy to build applications that react to on-chain events such as token transfers, contract state changes, or any other smart contract emissions.

## Features

- **Real-time Event Monitoring**: Subscribe to new blocks and process events as they happen
- **Historical Event Scanning**: Scan past events from a specified block number
- **Flexible Event Filtering**: Define custom filters for specific contracts and event signatures
- **Robust Error Handling**: Built-in retry mechanisms with configurable attempts and delays
- **Multiple Provider Support**: Connect via WebSocket or IPC
- **Async/Await Support**: Built with Tokio for high-performance async operations

## Status

This library is in early alpha stage. Expect breaking changes and bugs.