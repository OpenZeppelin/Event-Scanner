/// Proof that the scanner has been started.
///
/// This token is returned by [`EventScanner::start()`](crate::EventScanner) and must be passed to
/// [`EventSubscription::stream()`] to access the event stream. This ensures at compile
/// time that the scanner is started before attempting to read events.
///
/// # Example
///
/// ```ignore
/// let mut scanner = EventScannerBuilder::sync().from_block(0).connect(provider).await?;
/// let subscription = scanner.subscribe(filter);
///
/// // Start the scanner and get the token
/// let token = scanner.start().await?;
///
/// // Now we can access the stream
/// let mut stream = subscription.stream(&token);
/// ```
#[derive(Debug, Clone)]
pub struct ScannerToken {
    /// Private field prevents construction outside this crate
    _private: (),
}

impl ScannerToken {
    /// Creates a new scanner token.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self { _private: () }
    }
}
