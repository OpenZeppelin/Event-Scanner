//! Robust, retrying wrapper around Alloy providers.
//!
//! This module exposes [`RobustProvider`], a small wrapper around Alloy's
//! `RootProvider` that adds:
//! * bounded per-call timeouts
//! * exponential backoff retries
//! * transparent failover between a primary and one or more fallback providers
//! * more robust WebSocket block subscriptions with automatic reconnection
//!
//! Use [`RobustProviderBuilder`] to construct a provider with sensible defaults
//! and optional fallbacks, or implement the [`IntoRobustProvider`] and [`IntoProvider`]
//! traits to support custom providers.
//!
//! # How it works
//!
//! All RPC calls performed through [`RobustProvider`] are wrapped in a total
//! timeout (`call_timeout`) and retried with exponential backoff up to
//! `max_retries`. If the primary provider keeps failing, the call is retried
//! against the configured fallback providers in the order they were added. For subscriptions,
//! [`RobustSubscription`] also tracks lag, switches to fallbacks on repeated
//! failure, and periodically attempts to reconnect to the primary provider.
//!
//! # Examples
//!
//! Creating a robust WebSocket provider with an HTTP fallback and passing it to
//! the event scanner:
//!
//! ```rust,no_run
//! use alloy::providers::ProviderBuilder;
//! use event_scanner::{EventScannerBuilder, robust_provider::RobustProviderBuilder};
//! use std::time::Duration;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let ws = ProviderBuilder::new().connect("ws://localhost:8545").await?;
//! let http = ProviderBuilder::new().connect_http("http://localhost:8545".parse()?);
//!
//! let robust = RobustProviderBuilder::new(ws)
//!     .fallback(http)
//!     .call_timeout(Duration::from_secs(30))
//!     .subscription_timeout(Duration::from_secs(120))
//!     .build()
//!     .await?;
//!
//! let mut scanner = EventScannerBuilder::live().connect(robust);
//! // register filters and start the scanner...
//! # Ok(()) }
//! ```
//!
//! You can also convert existing providers using [`IntoRobustProvider`]

pub mod builder;
pub mod error;
pub mod provider;
pub mod provider_conversion;
pub mod subscription;

pub use builder::*;
pub use error::Error;
pub use provider::RobustProvider;
pub use provider_conversion::{IntoProvider, IntoRobustProvider};
pub use subscription::RobustSubscription;
