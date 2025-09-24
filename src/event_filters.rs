use std::sync::Arc;

use alloy::primitives::Address;

use crate::callback::EventCallback;

#[derive(Clone)]
pub struct EventFilter {
    /// Contract address to filter events from. If None, events from all contracts will be tracked.
    pub contract_address: Option<Address>,
    /// Human-readable event signature, e.g. "Transfer(address,address,uint256)".
    /// If None, all events from the specified contract(s) will be tracked.
    pub event: Option<String>,
    pub callback: Arc<dyn EventCallback + Send + Sync>,
}

impl EventFilter {
    /// Creates a new `EventFilter` builder.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use event_scanner::{EventFilter, EventCallback};
    /// use std::sync::Arc;
    /// use alloy::primitives::Address;
    ///
    /// # struct MyCallback;
    /// # #[async_trait::async_trait]
    /// # impl EventCallback for MyCallback {
    /// #     async fn on_event(&self, _log: &alloy::rpc::types::Log) -> anyhow::Result<()> { Ok(()) }
    /// # }
    /// # async fn example() -> anyhow::Result<()> {
    /// let contract_address = Address::ZERO;
    /// let callback = Arc::new(MyCallback);
    /// let filter = EventFilter::new()
    ///     .with_contract_address(contract_address)
    ///     .with_event("Transfer(address,address,uint256)")
    ///     .with_callback(callback);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> EventFilterBuilder {
        EventFilterBuilder::default()
    }
}

/// Builder for constructing `EventFilter` instances with optional fields.
#[derive(Default)]
pub struct EventFilterBuilder {
    contract_address: Option<Address>,
    event: Option<String>,
}

impl EventFilterBuilder {
    /// Sets the contract address to filter events from.
    /// If not set, events from all contracts will be tracked.
    #[must_use]
    pub fn with_contract_address(mut self, contract_address: Address) -> Self {
        self.contract_address = Some(contract_address);
        self
    }

    /// Sets the event signature to filter specific events.
    /// If not set, all events from the specified contract(s) will be tracked.
    #[must_use]
    pub fn with_event(mut self, event: impl Into<String>) -> Self {
        self.event = Some(event.into());
        self
    }

    /// Sets the callback for processing events and builds the `EventFilter`.
    ///
    /// # Panics
    ///
    /// Panics if the callback is not set, as it's required for event processing.
    #[must_use]
    pub fn with_callback(self, callback: Arc<dyn EventCallback + Send + Sync>) -> EventFilter {
        EventFilter { contract_address: self.contract_address, event: self.event, callback }
    }
}
