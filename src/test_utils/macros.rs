use alloy::primitives::LogData;
use tokio_stream::Stream;

use crate::{ScannerMessage, event_scanner::EventScannerResult};

#[macro_export]
macro_rules! assert_next {
    // 1. Explicit Error Matching (Value based) - uses the new PartialEq implementation
    ($stream: expr, Err($expected_err:expr)) => {
        $crate::assert_next!($stream, Err($expected_err), timeout = 5)
    };
    ($stream: expr, Err($expected_err:expr), timeout = $secs: expr) => {
        let message = tokio::time::timeout(
            std::time::Duration::from_secs($secs),
            tokio_stream::StreamExt::next(&mut $stream),
        )
        .await
        .expect("timed out");
        if let Some(msg) = message {
            let expected = &$expected_err;
            assert_eq!(&msg, expected, "Expected error {:?}, got {:?}", expected, msg);
        } else {
            panic!("Expected error {:?}, but channel was closed", $expected_err);
        }
    };

    // 2. Success Matching (Implicit unwrapping) - existing behavior
    ($stream: expr, $expected: expr) => {
        $crate::assert_next!($stream, $expected, timeout = 5)
    };
    ($stream: expr, $expected: expr, timeout = $secs: expr) => {
        let message = tokio::time::timeout(
            std::time::Duration::from_secs($secs),
            tokio_stream::StreamExt::next(&mut $stream),
        )
        .await
        .expect("timed out");
        let expected = $expected;
        match message {
            std::option::Option::Some(std::result::Result::Ok(msg)) => {
                assert_eq!(msg, expected, "Expected {:?}, got {:?}", expected, msg);
            }
            std::option::Option::Some(std::result::Result::Err(e)) => {
                panic!("Expected Ok({:?}), got Err({:?})", expected, e);
            }
            std::option::Option::None => {
                panic!("Expected Ok({:?}), but channel was closed", expected);
            }
        }
    };
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
/// ```no_run
/// # use alloy::sol;
/// sol! {
///     event CountIncreased(uint256 newCount);
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
/// * **Timeout**: The stream doesn't produce the next expected event within the timeout period
///   (default 5 seconds, configurable via `timeout = N` parameter).
/// * **Wrong event**: The stream emits a different event than the next expected one in the
///   sequence.
/// * **Extra events**: The stream emits more events than expected after the sequence completes.
/// * **Stream closed early**: The stream ends before all expected events are emitted.
/// * **Wrong message type**: The stream yields a non-`Data` message (e.g., `Error` or `Status`)
///   when an event is expected.
/// * **Empty sequence**: The macro is called with an empty event collection (use `assert_empty!`
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

/// Same as [`assert_event_sequence!`], but invokes [`assert_empty!`] at the end.
#[macro_export]
macro_rules! assert_event_sequence_final {
    // owned slices
    ($stream: expr, [$($event:expr),+ $(,)?]) => {{
        assert_event_sequence_final!($stream, &[$($event),+])
    }};
    ($stream: expr, [$($event:expr),+ $(,)?], timeout = $secs: expr) => {{
        assert_event_sequence_final!($stream, &[$($event),+], timeout = $secs)
    }};
    // borrowed slices
    ($stream: expr, &[$($event:expr),+ $(,)?]) => {{
        assert_event_sequence_final!($stream, &[$($event),+], timeout = 5)
    }};
    ($stream: expr, &[$($event:expr),+ $(,)?], timeout = $secs: expr) => {{
        $crate::assert_event_sequence!($stream, &[$($event),+], timeout = $secs);
        $crate::assert_empty!($stream)
    }};
    // variables and non-slice expressions
    ($stream: expr, $events: expr) => {{
        assert_event_sequence_final!($stream, $events, timeout = 5)
    }};
    ($stream: expr, $events: expr, timeout = $secs: expr) => {{
        $crate::assert_event_sequence!($stream, $events, timeout = $secs);
        $crate::assert_empty!($stream)
    }};
}

#[allow(clippy::missing_panics_doc)]
pub async fn assert_event_sequence<S: Stream<Item = EventScannerResult> + Unpin>(
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
            "Timed out waiting for events.\nNext Expected:\n{:#?}\nRemaining:\n{:#?}",
            expected,
            remaining.collect::<Vec<_>>()
        );

        let time_left = timeout_duration - elapsed;
        let message = tokio::time::timeout(time_left, tokio_stream::StreamExt::next(stream))
            .await
            .unwrap_or_else(|_| {
                panic!("timed out waiting for next stream batch, expected event: {expected:#?}")
            });

        match message {
            Some(Ok(ScannerMessage::Data(batch))) => {
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
            Some(Ok(other)) => {
                panic!("Expected Message::Data, got: {other:#?}");
            }
            Some(Err(e)) => {
                panic!("Expected Ok(Message::Data), got Err: {e:#?}");
            }
            None => {
                panic!("Stream closed while still expecting: {:#?}", remaining.collect::<Vec<_>>());
            }
        }
    }
}

/// Asserts that a stream of block ranges completely covers an expected block range.
///
/// This macro consumes messages from a stream and verifies that the block ranges received
/// sequentially cover the entire `expected_range` without gaps or overlaps. Each streamed
/// range must start exactly where the previous one ended, and all ranges must fit within
/// the expected bounds.
///
/// The macro expects the stream to yield `ScannerMessage::Data(range)` variants containing
/// `RangeInclusive<u64>` values representing block ranges. It tracks coverage by ensuring
/// each new range starts at the next expected block number and doesn't exceed the end of
/// the expected range. Once the entire range is covered, the assertion succeeds.
///
/// # Example
///
/// ```rust
/// use event_scanner::{ScannerMessage, assert_range_coverage};
/// use tokio::sync::mpsc;
/// use tokio_stream::wrappers::ReceiverStream;
///
/// #[tokio::test]
/// async fn test_scanner_covers_range() {
///     let (tx, rx) = mpsc::channel(10);
///     let mut stream = ReceiverStream::new(rx);
///
///     // Simulate a scanner that splits blocks 100-199 into chunks
///     tokio::spawn(async move {
///         tx.send(ScannerMessage::Data(100..=149)).await.unwrap();
///         tx.send(ScannerMessage::Data(150..=199)).await.unwrap();
///     });
///
///     // Assert that the stream covers blocks 100-199
///     assert_range_coverage!(stream, 100..=199);
/// }
/// ```
///
/// # Panics
///
/// * **Timeout**: The stream doesn't produce the next expected range within the timeout period
///   (default 5 seconds, configurable via `timeout = N` parameter).
/// * **Gap or overlap**: A streamed range doesn't start exactly at the next expected block number,
///   indicating a gap or overlap in coverage.
/// * **Out of bounds**: A streamed range extends beyond the end of the expected range.
/// * **Wrong message type**: The stream yields a non-`Data` message (e.g., `Error` or
///   `Notification`) when a block range is expected.
/// * **Stream closed early**: The stream ends before the entire expected range is covered.
///
/// On panic, the error message includes the expected remaining range and all previously
/// streamed ranges for debugging.
#[macro_export]
macro_rules! assert_range_coverage {
    ($stream: expr, $expected_range: expr) => {
        assert_range_coverage!($stream, $expected_range, timeout = 5)
    };
    ($stream: expr, $expected_range: expr, timeout = $secs: expr) => {{
        fn bounds<R: ::std::ops::RangeBounds<u64>>(range: &R) -> (u64, u64) {
            let start_bound = match range.start_bound() {
                ::std::ops::Bound::Unbounded => 0,
                ::std::ops::Bound::Excluded(&x) => x + 1,
                ::std::ops::Bound::Included(&x) => x,
            };
            let end_bound = match range.end_bound() {
                ::std::ops::Bound::Unbounded => u64::MAX,
                ::std::ops::Bound::Excluded(&x) => x - 1,
                ::std::ops::Bound::Included(&x) => x,
            };
            (start_bound, end_bound)
        }

        let original_bounds = bounds(&$expected_range);
        let (mut start, end) = original_bounds;

        let start_time = ::std::time::Instant::now();
        let timeout_duration = ::std::time::Duration::from_secs($secs);

        // log all streamed ranges on failures
        let mut streamed_ranges = vec![];

        while start <= end {
            let elapsed = start_time.elapsed();

            assert!(elapsed < timeout_duration, "Timed out. Still expecting: {:#?}", start..=end,);

            let time_left = timeout_duration - elapsed;
            let message =
                tokio::time::timeout(time_left, tokio_stream::StreamExt::next(&mut $stream))
                    .await
                    .expect("Timed out waiting for the next block range");

            match message {
                std::option::Option::Some(std::result::Result::Ok(event_scanner::ScannerMessage::Data(range))) => {
                    let (streamed_start, streamed_end) = bounds(&range);
                    streamed_ranges.push(range.clone());
                    assert!(
                        start == streamed_start && streamed_end <= end,
                        "Unexpected range bounds, expected max. range: {:#?}, got: {:#?}\nPrevious streams:\n{:#?}",
                        start..=end,
                        range,
                        streamed_ranges,
                    );
                    start = streamed_end + 1;
                }
                std::option::Option::Some(std::result::Result::Ok(other)) => {
                    panic!("Expected a block range, got: {other:#?}");
                }
                std::option::Option::Some(std::result::Result::Err(e)) => {
                    panic!("Expected Ok(Message::Data), got Err: {e:#?}");
                }
                std::option::Option::None => {
                    panic!("Stream closed without covering range: {:#?}", start..=end);
                }
            }
        }
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
