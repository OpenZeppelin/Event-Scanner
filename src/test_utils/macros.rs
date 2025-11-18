use alloy::primitives::LogData;
use tokio_stream::Stream;

use crate::Message;

#[macro_export]
macro_rules! assert_next {
    ($stream: expr, $expected: expr) => {
        assert_next!($stream, $expected, timeout = 5)
    };
    ($stream: expr, $expected: expr, timeout = $secs: expr) => {
        let message = tokio::time::timeout(
            std::time::Duration::from_secs($secs),
            tokio_stream::StreamExt::next(&mut $stream),
        )
        .await
        .expect("timed out");
        if let Some(msg) = message {
            assert_eq!(msg, $expected)
        } else {
            panic!("Expected {:?}, but channel was closed", $expected)
        }
    };
}

/// Asserts that a stream emits a specific sequence of events in order.
///
/// This macro consumes messages from a stream and verifies that the provided events are emitted
/// in the exact order specified, regardless of how they are batched. The stream may emit events
/// across multiple batches or all at once—the macro handles both cases. It ensures no unexpected
/// events appear between the expected ones and that the sequence completes exactly as specified.
///
/// The macro accepts events of any type implementing [`SolEvent`](alloy::sol_types::SolEvent).
/// Events are compared by their encoded log data, allowing flexible matching across different
/// batch configurations while maintaining strict ordering requirements.
///
/// # Examples
///
/// ```
/// sol! {
///     event CountIncreased {
///         uint256 newCount;
///     }
/// }
///
/// #[tokio::test]
/// async fn test_event_order() {
///     // scanner setup...
///
///     let mut stream = scanner.subscribe(EventFilter::new().contract_address(contract_address));
///
///     // Assert these two events are emitted in order
///     assert_event_sequence!(
///         stream,
///         &[
///             CountIncreased { newCount: U256::from(1) },
///             CountIncreased { newCount: U256::from(2) },
///         ]
///     );
/// }
/// ```
///
/// The assertion passes whether events arrive in separate batches or together:
/// * **Separate batches**: `[Event1]`, then `[Event2]`
/// * **Single batch**: `[Event1, Event2]`
///
/// # Panics
///
/// - **Timeout**: The stream doesn't produce the next expected event within the timeout period
///   (default 5 seconds, configurable via `timeout = N` parameter).
/// - **Wrong event**: The stream emits a different event than the next expected one in the
///   sequence.
/// - **Extra events**: The stream emits more events than expected after the sequence completes.
/// - **Stream closed early**: The stream ends before all expected events are emitted.
/// - **Wrong message type**: The stream yields a non-`Data` message (e.g., `Error` or `Status`)
///   when an event is expected.
/// - **Empty sequence**: The macro is called with an empty event collection (use `assert_empty!`
///   instead).
///
/// On panic, the error message includes the remaining expected events for debugging.
#[macro_export]
macro_rules! assert_event_sequence {
    // owned slices just pass to the borrowed slices variant
    ($stream: expr, [$($event:expr),+ $(,)?]) => {
        assert_event_sequence!($stream, &[$($event),+], timeout = 5)
    };
    ($stream: expr, [$($event:expr),+ $(,)?], timeout = $secs: expr) => {
        assert_event_sequence!($stream, &[$($event),+], timeout = $secs)
    };
    // borrowed slices
    ($stream: expr, &[$($event:expr),+ $(,)?]) => {
        assert_event_sequence!($stream, &[$($event),+], timeout = 5)
    };
    ($stream: expr, &[$($event:expr),+ $(,)?], timeout = $secs: expr) => {
        let expected_options = &[$(alloy::sol_types::SolEvent::encode_log_data(&$event)),+];

       $crate::test_utils::macros::assert_event_sequence(&mut $stream, expected_options, $secs).await
    };
    // variables and non-slice expressions
    ($stream: expr, $events: expr) => {
        assert_event_sequence!($stream, $events, timeout = 5)
    };
    ($stream: expr, $events: expr, timeout = $secs: expr) => {
        let expected_options = $events.iter().map(alloy::sol_types::SolEvent::encode_log_data).collect::<Vec<_>>();
        if expected_options.is_empty() {
            panic!("error: assert_event_sequence! called with an empty collection. Use assert_empty! macro instead to check for no pending messages.")
        }
        $crate::test_utils::macros::assert_event_sequence(&mut $stream, expected_options.iter(), $secs).await
    };
}

#[allow(clippy::missing_panics_doc)]
pub async fn assert_event_sequence<S: Stream<Item = Message> + Unpin>(
    stream: &mut S,
    expected_options: impl IntoIterator<Item = &LogData>,
    timeout_secs: u64,
) {
    let mut remaining = expected_options.into_iter();
    let start = std::time::Instant::now();
    let timeout_duration = std::time::Duration::from_secs(timeout_secs);

    while let Some(expected) = remaining.next() {
        let elapsed = start.elapsed();

        assert!(
            elapsed < timeout_duration,
            "Timed out waiting for events. Still expecting: {:#?}",
            remaining.collect::<Vec<_>>()
        );

        let time_left = timeout_duration - elapsed;
        let message = tokio::time::timeout(time_left, tokio_stream::StreamExt::next(stream))
            .await
            .expect("timed out waiting for next batch");

        match message {
            Some(Message::Data(batch)) => {
                let mut batch = batch.iter();
                let event = batch.next().expect("Streamed batch should not be empty");
                assert_eq!(
                    expected,
                    event.data(),
                    "\nRemaining: {:#?}\n",
                    remaining.collect::<Vec<_>>()
                );
                while let Some(event) = batch.next() {
                    let expected = remaining.next().unwrap_or_else(|| panic!("Received more events than expected.\nNext event: {:#?}\nStreamed remaining: {batch:#?}", event.data()));
                    assert_eq!(
                        expected,
                        event.data(),
                        "\nRemaining: {:#?}\n",
                        remaining.collect::<Vec<_>>()
                    );
                }
            }
            Some(other) => {
                panic!("Expected Message::Data, got: {other:#?}");
            }
            None => {
                panic!("Stream closed while still expecting: {:#?}", remaining.collect::<Vec<_>>());
            }
        }
    }
}

#[macro_export]
macro_rules! assert_closed {
    ($stream: expr) => {
        assert_closed!($stream, timeout = 5)
    };
    ($stream: expr, timeout = $secs: expr) => {
        let message = tokio::time::timeout(
            std::time::Duration::from_secs($secs),
            tokio_stream::StreamExt::next(&mut $stream),
        )
        .await
        .expect("timed out");
        assert!(message.is_none())
    };
}

#[macro_export]
macro_rules! assert_empty {
    ($stream: expr) => {{
        let inner = $stream.into_inner();
        assert!(inner.is_empty(), "Stream should have no pending messages");
        tokio_stream::wrappers::ReceiverStream::new(inner)
    }};
}

#[cfg(test)]
mod tests {
    use alloy::sol;
    use tokio::sync::mpsc;
    use tokio_stream::wrappers::ReceiverStream;

    sol! {
        #[derive(Debug)]
        event Transfer(address indexed from, address indexed to, uint256 value);
    }

    #[tokio::test]
    #[should_panic = "error: assert_event_sequence! called with an empty collection. Use assert_empty! macro instead to check for no pending messages."]
    async fn assert_event_sequence_macro_with_empty_vec() {
        let (_tx, rx) = mpsc::channel(10);
        let mut stream = ReceiverStream::new(rx);

        let empty_vec: Vec<Transfer> = Vec::new();
        assert_event_sequence!(stream, empty_vec);
    }

    #[tokio::test]
    #[should_panic = "error: assert_event_sequence! called with an empty collection. Use assert_empty! macro instead to check for no pending messages."]
    async fn assert_event_sequence_macro_with_empty_slice() {
        let (_tx, rx) = mpsc::channel(10);
        let mut stream = ReceiverStream::new(rx);

        let empty_vec: &[Transfer] = &[];
        assert_event_sequence!(stream, empty_vec);
    }
}
