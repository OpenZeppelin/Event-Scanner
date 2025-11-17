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

// TODO: implement assert_range_coverage

/// Asserts that the stream emits the given events in order.
///
/// The macro asserts that all of the provided events are emitted in the order they are provided, no
/// matter how many stream batches are necessary to emit them, that no other events are emitted in
/// between, and that the final event is the last event in the last batch of the stream.
///
/// The provided events can be of any type that implements [`SolEvent`](alloy::sol_types::SolEvent).
///
/// # Examples
///
/// ```
/// let mut stream = scanner.subscribe(EventFilter::new().contract_address(contract_address));
/// assert_event_sequence!(
///     stream,
///     &[CountIncreased { newCount: U256::from(1) }, CountIncreased { newCount: U256::from(2) },]
/// );
/// ```
///
/// The above assertion will pass if next stream batches are any of the following:
///
/// 1. Each event separate:
///     - Batch 1: `[CountIncreased { newCount: U256::from(1) }]`
///     - Batch 2: `[CountIncreased { newCount: U256::from(2) }]`
/// 2. Events in the same batch:
///     - Batch 1: `[CountIncreased { newCount: U256::from(1) }, CountIncreased { newCount:
///    U256::from(2) }]`
///
/// # Panics
///
/// * If the stream emits a different event than the next expected one
/// * If the stream is closed before all of the expected events are emitted
/// * If the stream emits more events than expected
/// * If the stream times out before all of the expected events are emitted
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
        let expected_options = &[
            $(
                {
                    let event = &$event;
                    let encoded_data = alloy::sol_types::SolEvent::encode_log_data(event);
                    let debug_string = format!("{:#?}", event);
                    (encoded_data, debug_string)
                }
            ),+
        ];

       $crate::test_utils::macros::assert_event_sequence(&mut $stream, expected_options, $secs).await
    };
    // variables and non-slice expressions
    ($stream: expr, $events: expr) => {
        assert_event_sequence!($stream, $events, timeout = 5)
    };
    ($stream: expr, $events: expr, timeout = $secs: expr) => {
        let expected_options = $events.iter().map(|e| (alloy::sol_types::SolEvent::encode_log_data(e), format!("{e:#?}"))).collect::<Vec<_>>();
        $crate::test_utils::macros::assert_event_sequence(&mut $stream, expected_options.iter(), $secs).await
    };
}

#[allow(clippy::missing_panics_doc)]
pub async fn assert_event_sequence<S: Stream<Item = Message> + Unpin>(
    stream: &mut S,
    expected_options: impl IntoIterator<Item = &(LogData, String)>,
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
                    &expected.0,
                    event.data(),
                    "Unexpected event: {event:#?}\nExpected: {}\nRemaining: {:#?}",
                    expected.1,
                    remaining.collect::<Vec<_>>()
                );
                while let Some(event) = batch.next() {
                    let expected = remaining.next().unwrap_or_else(|| panic!("Received more events than expected, current: {event:#?}\nStreamed batch: {batch:#?}"));
                    assert_eq!(
                        &expected.0,
                        event.data(),
                        "Unexpected event: {event:#?}\nExpected: {}\nRemaining: {:#?}",
                        expected.1,
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
