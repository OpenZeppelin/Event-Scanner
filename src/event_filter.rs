use std::fmt::Display;

use alloy::primitives::Address;

/// Type representing filters to apply when fetching events from the chain.
///
/// # Examples
///
/// ```rust
/// use alloy::primitives::address;
/// use event_scanner::EventFilter;
///
/// pub async fn create_event_filter() -> EventFilter {
///     let contract_address = address!("0xd8dA6BF26964af9d7eed9e03e53415d37aa96045");
///     let filter = EventFilter::new()
///         .with_contract_address(contract_address)
///         .with_event("Transfer(address,address,uint256)");
///
///     filter
/// }
/// ```
#[derive(Clone, Default)]
pub struct EventFilter {
    /// Contract address to filter events from. If None, events from all contracts will be tracked.
    pub(crate) contract_address: Option<Address>,
    /// Human-readable event signature, e.g. "Transfer(address,address,uint256)".
    /// If None, all events from the specified contract(s) will be tracked.
    pub(crate) events: Vec<String>,
}

impl Display for EventFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let address = self
            .contract_address
            .map(|addr| format!("{addr:?}"))
            .unwrap_or("all contracts".to_string());
        let events =
            if self.events.is_empty() { "all events".to_string() } else { self.events.join(", ") };

        write!(f, "EventFilter(contract: {}, events: {})", address, events)
    }
}

impl EventFilter {
    /// Creates a new [`EventFilter`].
    #[must_use]
    pub fn new() -> Self {
        EventFilter::default()
    }

    /// Sets the contract address to filter events from.
    /// If not set, events from all contracts will be tracked.
    #[must_use]
    pub fn with_contract_address(mut self, contract_address: impl Into<Address>) -> Self {
        self.contract_address = Some(contract_address.into());
        self
    }

    /// Sets the event signature to filter specific events.
    /// If not set, all events from the specified contract(s) will be tracked.
    #[must_use]
    pub fn with_event(mut self, event: impl Into<String>) -> Self {
        self.events.push(event.into());
        self
    }

    /// Sets the event signature to filter specific events.
    /// If not set, all events from the specified contract(s) will be tracked.
    #[must_use]
    pub fn with_events(mut self, events: impl IntoIterator<Item = String>) -> Self {
        self.events.extend(events);
        self
    }
}
