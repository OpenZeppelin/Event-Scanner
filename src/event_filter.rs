use std::fmt::{Debug, Display};

use alloy::{
    primitives::{Address, keccak256},
    rpc::types::{Filter, Topic, ValueOrArray},
};

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
    /// Human-readable event signatures, e.g. "Transfer(address,address,uint256)".
    pub(crate) events: Vec<String>,
    /// Event signature hashes, e.g. `0x123...`.
    pub(crate) event_signatures: Topic,
}

impl Display for EventFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut content = vec![];
        if let Some(address) = &self.contract_address {
            content.push(format!("contract: {address}"));
        }
        if !self.events.is_empty() {
            content.push(format!("events: [{}]", self.events.join(", ")));
        }
        if !self.event_signatures.is_empty() {
            // No guarantee the order of values returned by `Topic`
            let value_or_array = self.event_signatures.to_value_or_array().unwrap();
            let event_signatures = match value_or_array {
                ValueOrArray::Value(value) => format!("{value}"),
                ValueOrArray::Array(arr) => {
                    arr.iter().map(|t| format!("{t}")).collect::<Vec<_>>().join(", ")
                }
            };
            content.push(format!("event_signatures: [{event_signatures}]"));
        }

        write!(f, "EventFilter({})", content.join(", "))
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
    /// If neither events nor event signature hashes are set, all events from the specified contract(s) will be tracked.
    #[must_use]
    pub fn with_event(mut self, event: impl Into<String>) -> Self {
        let event = event.into();
        if !event.is_empty() {
            self.events.push(event);
        }
        self
    }

    /// Sets the event signatures to filter specific events.
    /// If neither events nor event signature hashes are set, all events from the specified contract(s) will be tracked.
    #[must_use]
    pub fn with_events(mut self, events: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.events.extend(events.into_iter().map(Into::into));
        self
    }

    /// Sets the event signature hash to filter specific events.
    /// If neither event signature hashes nor events are set, all events from the specified contract(s) will be tracked.
    #[must_use]
    pub fn with_event_signature(mut self, event_signature: impl Into<Topic>) -> Self {
        self.event_signatures = self.event_signatures.extend(event_signature.into());
        self
    }

    /// Sets the event signature hashes to filter specific events.
    /// If neither event signature hashes nor events are set, all events from the specified contract(s) will be tracked.
    #[must_use]
    pub fn with_event_signatures(
        mut self,
        event_signatures: impl IntoIterator<Item = impl Into<Topic>>,
    ) -> Self {
        for event_signature in event_signatures {
            self.event_signatures = self.event_signatures.extend(event_signature);
        }
        self
    }

    /// Returns a [`Topic`] containing all event signature hashes.
    /// If neither events nor event signature hashes are set, an empty [`Topic`] is returned.
    #[must_use]
    pub fn all_events(&self) -> Topic {
        let events = self.events.iter().map(|e| keccak256(e.as_bytes())).collect::<Vec<_>>();
        let sigs = self.event_signatures.clone();
        let sigs = sigs.extend(events);
        sigs
    }
}

impl From<EventFilter> for Filter {
    fn from(value: EventFilter) -> Self {
        let mut filter = Filter::new();
        if let Some(contract_address) = value.contract_address {
            filter = filter.address(contract_address);
        }
        let events = value.all_events();
        if !events.is_empty() {
            filter = filter.event_signature(events);
        }
        filter
    }
}

impl From<&EventFilter> for Filter {
    fn from(value: &EventFilter) -> Self {
        let mut filter = Filter::new();
        if let Some(contract_address) = value.contract_address {
            filter = filter.address(contract_address);
        }
        let events = value.all_events();
        if !events.is_empty() {
            filter = filter.event_signature(events);
        }
        filter
    }
}

#[cfg(test)]
mod tests {
    use super::EventFilter;
    use alloy::{primitives::address, sol, sol_types::SolEvent};

    sol! {
        contract SomeContract {
            event EventOne();
            event EventTwo();
        }
    }

    #[test]
    fn display_default_no_address_no_events() {
        let filter = EventFilter::new();
        let got = format!("{filter}");
        let expected = "EventFilter()";
        assert_eq!(got, expected);

        // Debug should equal Display
        assert_eq!(format!("{filter:?}"), got);
    }

    #[test]
    fn display_with_address() {
        let address = address!("0x000000000000000000000000000000000000dEaD");
        let filter = EventFilter::new().with_contract_address(address);
        let got = format!("{filter}");
        let expected = format!("EventFilter(contract: 0x000000000000000000000000000000000000dEaD)");
        assert_eq!(got, expected);

        // Debug should equal Display
        assert_eq!(format!("{filter:?}"), got);
    }

    #[test]
    fn display_single_event() {
        let event = SomeContract::EventOne::SIGNATURE;
        let filter = EventFilter::new().with_event(event);
        let got = format!("{filter}");
        let expected = format!("EventFilter(events: [EventOne()])");
        assert_eq!(got, expected);

        // Debug should equal Display
        assert_eq!(format!("{filter:?}"), got);
    }

    #[test]
    fn display_single_event_signature() {
        let event_signature = SomeContract::EventOne::SIGNATURE_HASH;
        let filter = EventFilter::new().with_event_signature(event_signature);
        let got = format!("{filter}");
        let expected = format!(
            "EventFilter(event_signatures: [0xa08dd6fd0d644da5df33d075cb9256203802f6948ab81b87079960711810dc91])"
        );
        assert_eq!(got, expected);

        // Debug should equal Display
        assert_eq!(format!("{filter:?}"), got);
    }

    #[test]
    fn display_multiple_events_with_address() {
        let address = address!("0x000000000000000000000000000000000000dEaD");
        let events = vec![SomeContract::EventOne::SIGNATURE, SomeContract::EventTwo::SIGNATURE];
        let filter = EventFilter::new().with_contract_address(address).with_events(events.clone());

        let got = format!("{filter}");
        let expected = format!(
            "EventFilter(contract: 0x000000000000000000000000000000000000dEaD, events: [EventOne(), EventTwo()])"
        );
        assert_eq!(got, expected);

        // Debug should equal Display
        assert_eq!(format!("{filter:?}"), got);
    }

    #[test]
    fn display_multiple_event_signatures_with_address() {
        let address = address!("0x000000000000000000000000000000000000dEaD");
        let event_signatures =
            vec![SomeContract::EventOne::SIGNATURE_HASH, SomeContract::EventTwo::SIGNATURE_HASH];
        let filter = EventFilter::new()
            .with_contract_address(address)
            .with_event_signatures(event_signatures.clone());

        let got = format!("{filter}");
        // `Topic` doesn't guarantee the order of values it returns
        let expected_1 = format!(
            "EventFilter(contract: 0x000000000000000000000000000000000000dEaD, event_signatures: [0x16eb4fc7651e068f1c31303645026f82d5fced11a8d5209bbf272072be23ddff, 0xa08dd6fd0d644da5df33d075cb9256203802f6948ab81b87079960711810dc91])"
        );
        let expected_2 = format!(
            "EventFilter(contract: 0x000000000000000000000000000000000000dEaD, event_signatures: [0xa08dd6fd0d644da5df33d075cb9256203802f6948ab81b87079960711810dc91, 0x16eb4fc7651e068f1c31303645026f82d5fced11a8d5209bbf272072be23ddff])"
        );
        assert!(
            got == expected_1 || got == expected_2,
            "got: {got},\nexpected_1: {expected_1},\nexpected_2: {expected_2}"
        );

        // Debug should equal Display
        assert_eq!(format!("{filter:?}"), got);
    }

    #[test]
    fn display_multiple_events_and_event_signatures() {
        let events = vec![SomeContract::EventOne::SIGNATURE, SomeContract::EventTwo::SIGNATURE];
        let event_signatures =
            vec![SomeContract::EventOne::SIGNATURE_HASH, SomeContract::EventTwo::SIGNATURE_HASH];
        let filter = EventFilter::new()
            .with_events(events.clone())
            .with_event_signatures(event_signatures.clone());

        let got = format!("{filter}");
        let expected_1 = format!(
            "EventFilter(events: [EventOne(), EventTwo()], event_signatures: [0x16eb4fc7651e068f1c31303645026f82d5fced11a8d5209bbf272072be23ddff, 0xa08dd6fd0d644da5df33d075cb9256203802f6948ab81b87079960711810dc91])",
        );
        let expected_2 = format!(
            "EventFilter(events: [EventOne(), EventTwo()], event_signatures: [0xa08dd6fd0d644da5df33d075cb9256203802f6948ab81b87079960711810dc91, 0x16eb4fc7651e068f1c31303645026f82d5fced11a8d5209bbf272072be23ddff])",
        );
        assert!(
            got == expected_1 || got == expected_2,
            "got: {got},\nexpected_1: {expected_1},\nexpected_2: {expected_2}"
        );

        // Debug should equal Display
        assert_eq!(format!("{filter:?}"), got);
    }

    #[test]
    fn display_multiple_events_and_event_signatures_with_address() {
        let address = address!("0x000000000000000000000000000000000000dEaD");
        let events = vec![SomeContract::EventOne::SIGNATURE, SomeContract::EventTwo::SIGNATURE];
        let event_signatures =
            vec![SomeContract::EventOne::SIGNATURE_HASH, SomeContract::EventTwo::SIGNATURE_HASH];
        let filter = EventFilter::new()
            .with_contract_address(address)
            .with_events(events.clone())
            .with_event_signatures(event_signatures.clone());

        let got = format!("{filter}");
        let expected_1 = format!(
            "EventFilter(contract: 0x000000000000000000000000000000000000dEaD, events: [EventOne(), EventTwo()], event_signatures: [0x16eb4fc7651e068f1c31303645026f82d5fced11a8d5209bbf272072be23ddff, 0xa08dd6fd0d644da5df33d075cb9256203802f6948ab81b87079960711810dc91])",
        );
        let expected_2 = format!(
            "EventFilter(contract: 0x000000000000000000000000000000000000dEaD, events: [EventOne(), EventTwo()], event_signatures: [0xa08dd6fd0d644da5df33d075cb9256203802f6948ab81b87079960711810dc91, 0x16eb4fc7651e068f1c31303645026f82d5fced11a8d5209bbf272072be23ddff])",
        );
        assert!(
            got == expected_1 || got == expected_2,
            "got: {got},\nexpected_1: {expected_1},\nexpected_2: {expected_2}"
        );

        // Debug should equal Display
        assert_eq!(format!("{filter:?}"), got);
    }

    #[test]
    fn display_with_empty_events_vector_noop() {
        // Providing an empty events vector should behave as if no events were set.
        let filter = EventFilter::new().with_events(Vec::<String>::new());
        let got = format!("{filter}");
        let expected = "EventFilter()";
        assert_eq!(got, expected);

        // Debug should equal Display
        assert_eq!(format!("{filter:?}"), got);
    }
}
