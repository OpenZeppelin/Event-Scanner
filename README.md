# Event Scanner

> ⚠️ **WARNING: ACTIVE DEVELOPMENT** ⚠️
> 
> This project is under active development and likely contains many bugs. Use at your own risk!
> APIs and functionality may change without notice.

## About

This is an ethereum L1 event scanner is a Rust-based Ethereum blockchain event monitoring library built on top of the Alloy framework. It provides a flexible and efficient way to:

- Subscribe to and monitor specific smart contract events in real-time
- Process historical events from a specified starting block
- Handle event callbacks with configurable retry logic
- Support both WebSocket and IPC connections to Ethereum nodes

The scanner allows you to define custom event filters with associated callbacks, making it easy to build applications that react to on-chain events, specifically with rollups in mind. This is varies from traditional indexers in that all logic is handlded in memory and no database is used. 


## Status

This library is in early alpha stage. Expect breaking changes and bugs.
