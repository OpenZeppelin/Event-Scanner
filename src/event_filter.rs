use alloy::primitives::Address;

#[derive(Clone, Default)]
pub struct EventFilter {
    /// Contract address to filter events from. If None, events from all contracts will be tracked.
    pub contract_address: Option<Address>,
    /// Human-readable event signature, e.g. "Transfer(address,address,uint256)".
    /// If None, all events from the specified contract(s) will be tracked.
    pub event: Option<String>,
}

impl EventFilter {
    /// Creates a new [`EventFilter`].
    ///
    /// # Examples
    ///
    /// ```rust
    /// use alloy::primitives::Address;
    /// use event_scanner::EventFilter;
    ///
    /// pub async fn create_event_filter() -> EventFilter {
    ///     let contract_address = Address::ZERO;
    ///     let filter = EventFilter::new()
    ///         .with_contract_address(contract_address)
    ///         .with_event("Transfer(address,address,uint256)");
    ///
    ///     filter
    /// }
    /// ```
    #[must_use]
    pub fn new() -> Self {
        EventFilter::default()
    }

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
    pub fn with_event<E: Into<String>>(mut self, event: E) -> Self {
        self.event = Some(event.into());
        self
    }
}
