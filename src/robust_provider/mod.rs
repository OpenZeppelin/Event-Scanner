//! Robust, retrying wrapper around Alloy providers.
//!
//! This module exposes [`RobustProvider`], a small wrapper around Alloy's
//! [`RootProvider`](alloy::providers::RootProvider) that adds:
//! * bounded per-call timeouts
//! * exponential backoff retries
//! * transparent failover between a primary and one or more fallback providers
//! * more robust WebSocket block subscriptions with automatic reconnection
//!
//! Use [`RobustProviderBuilder`] to construct a provider with sensible defaults
//! and optional fallbacks, or implement the [`IntoRobustProvider`] and [`IntoRootProvider`]
//! traits to support custom providers.
//!
//! # How it works
//!
//! All RPC calls performed through [`RobustProvider`] are wrapped in a total
//! timeout and retried with exponential backoff up to `max_retries`. If the
//! primary provider keeps failing, the call is retried against the configured
//! fallback providers in the order they were added. For subscriptions,
//! [`RobustSubscription`] also tracks lag, switches to fallbacks on repeated
//! failure, and periodically attempts to reconnect to the primary provider.
//!
//! # Examples
//!
//! Creating a robust WebSocket provider with a fallback:
//!
//! ```rust,no_run
//! use alloy::providers::{Provider, ProviderBuilder};
//! use event_scanner::robust_provider::RobustProviderBuilder;
//! use std::time::Duration;
//! use tokio_stream::StreamExt;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let ws = ProviderBuilder::new().connect("ws://localhost:8545").await?;
//! let ws_fallback = ProviderBuilder::new().connect("ws://localhost:8456").await?;
//!
//! let robust = RobustProviderBuilder::new(ws)
//!     .fallback(ws_fallback)
//!     .call_timeout(Duration::from_secs(30))
//!     .subscription_timeout(Duration::from_secs(120))
//!     .build()
//!     .await?;
//!
//! // Make RPC calls with automatic retries and fallback
//! let block_number = robust.get_block_number().await?;
//! println!("Current block: {}", block_number);
//!
//! // Create subscriptions that automatically reconnect on failure
//! let sub = robust.subscribe_blocks().await?;
//! let mut stream = sub.into_stream();
//! while let Some(response) = stream.next().await {
//!     match response {
//!         Ok(block) => println!("New block: {:?}", block),
//!         Err(e) => println!("Got error: {:?}", e),
//!     }
//! }
//! # Ok(()) }
//! ```
//!
//! You can also convert existing providers using [`IntoRobustProvider`]

pub mod builder;
pub mod provider;
pub mod provider_conversion;
pub mod subscription;

pub use builder::*;
pub use provider::{Error, RobustProvider};
pub use provider_conversion::{IntoRobustProvider, IntoRootProvider};
pub use subscription::{DEFAULT_RECONNECT_INTERVAL, RobustSubscription};
