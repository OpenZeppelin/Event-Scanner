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
            panic!("Expected {:?}, got: {message:?}", $expected)
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

/// Asserts that a stream of block ranges completely covers an expected block range.
///
/// This macro consumes messages from a stream and verifies that the block ranges received
/// sequentially cover the entire `expected_range` without gaps or overlaps. Each streamed
/// range must start exactly where the previous one ended, and all ranges must fit within
/// the expected bounds.
///
/// The macro expects the stream to yield `Message::Data(range)` variants containing
/// `RangeInclusive<u64>` values representing block ranges. It tracks coverage by ensuring
/// each new range starts at the next expected block number and doesn't exceed the end of
/// the expected range. Once the entire range is covered, the assertion succeeds.
///
/// # Example
///
/// ```rust
/// use event_scanner::{assert_range_coverage, block_range_scanner::Message};
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
///         tx.send(Message::Data(100..=149)).await.unwrap();
///         tx.send(Message::Data(150..=199)).await.unwrap();
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
/// * **Wrong message type**: The stream yields a non-`Data` message (e.g., `Error` or `Status`)
///   when a block range is expected.
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
                Some( $crate::block_range_scanner::Message::Data(range)) => {
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
                Some(other) => {
                    panic!("Expected a block range, got: {other:#?}");
                }
                None => {
                    panic!("Stream closed without covering range: {:#?}", start..=end);
                }
            }
        }
    }};
}
