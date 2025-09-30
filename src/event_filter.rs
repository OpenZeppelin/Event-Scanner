use std::fmt::{Debug, Display};

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
        let address =
            self.contract_address.map_or("all contracts".to_string(), |addr| format!("{addr:?}"));
        let events =
            if self.events.is_empty() { "all events".to_string() } else { self.events.join(", ") };

        write!(f, "EventFilter(contract: {address}, events: {events})")
    }
}

impl Debug for EventFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self}")
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

#[cfg(test)]
mod tests {
    use super::EventFilter;
    use alloy::primitives::{Address, address};

    fn addr_str(addr: Address) -> String {
        format!("{:?}", addr)
    }

    #[test]
    fn display_default_no_address_no_events() {
        let filter = EventFilter::new();
        let got = format!("{}", filter);
        let expected = "EventFilter(contract: all contracts, events: all events)";
        assert_eq!(got, expected);

        // Debug should equal Display
        assert_eq!(format!("{:?}", filter), got);
    }

    #[test]
    fn display_with_address_no_events() {
        let address = address!("0xd8dA6BF26964af9d7eed9e03e53415d37aa96045");
        let filter = EventFilter::new().with_contract_address(address);
        let got = format!("{}", filter);
        let expected = format!("EventFilter(contract: {}, events: all events)", addr_str(address));
        assert_eq!(got, expected);

        // Debug should equal Display
        assert_eq!(format!("{:?}", filter), got);
    }

    #[test]
    fn display_single_event_no_address() {
        let event = "Transfer(address,address,uint256)";
        let filter = EventFilter::new().with_event(event);
        let got = format!("{}", filter);
        let expected = format!("EventFilter(contract: all contracts, events: {})", event);
        assert_eq!(got, expected);

        // Debug should equal Display
        assert_eq!(format!("{:?}", filter), got);
    }

    #[test]
    fn display_multiple_events_with_address() {
        let address = address!("0x000000000000000000000000000000000000dEaD");
        let events = vec![
            "Transfer(address,address,uint256)".to_string(),
            "Approval(address,address,uint256)".to_string(),
            "Sync(uint112,uint112)".to_string(),
        ];
        let filter = EventFilter::new().with_contract_address(address).with_events(events.clone());

        let got = format!("{}", filter);
        let expected =
            format!("EventFilter(contract: {}, events: {})", addr_str(address), events.join(", "));
        assert_eq!(got, expected);

        // Debug should equal Display
        assert_eq!(format!("{:?}", filter), got);
    }

    #[test]
    fn display_with_empty_events_vector_noop() {
        // Providing an empty events vector should behave as if no events were set.
        let filter = EventFilter::new().with_events(Vec::<String>::new());
        let got = format!("{}", filter);
        let expected = "EventFilter(contract: all contracts, events: all events)";
        assert_eq!(got, expected);

        // Debug should equal Display
        assert_eq!(format!("{:?}", filter), got);
    }

    #[test]
    fn display_with_empty_event_string_prints_empty() {
        // An explicitly empty event string results in an empty events field when joined.
        let filter = EventFilter::new().with_event("");
        let got = format!("{}", filter);
        let expected = "EventFilter(contract: all contracts, events: )";
        assert_eq!(got, expected);

        // Debug should equal Display
        assert_eq!(format!("{:?}", filter), got);
    }
}
