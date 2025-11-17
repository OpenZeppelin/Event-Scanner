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

#[macro_export]
macro_rules! assert_event_sequence {
    ($stream: expr, $expected_options: expr) => {
        assert_event_sequence!($stream, $expected_options, timeout = 5)
    };
    ($stream: expr, $expected_options: expr, timeout = $secs: expr) => {
        let expected_options = $expected_options;
        if expected_options.is_empty() {
            panic!("assert_event_sequence! called with empty array. Use assert_empty! macro instead to check for no pending messages.");
        }

        let mut remaining = expected_options.iter();
        let start = std::time::Instant::now();
        let timeout_duration = std::time::Duration::from_secs($secs);

        while let Some(expected) = remaining.next() {
            let elapsed = start.elapsed();
            if elapsed >= timeout_duration {
                panic!("Timed out waiting for events. Still expecting: {:#?}", remaining);
            }

            let time_left = timeout_duration - elapsed;
            let message =
                tokio::time::timeout(time_left, tokio_stream::StreamExt::next(&mut $stream))
                    .await
                    .expect("timed out waiting for next batch");

            match message {
                Some($crate::ScannerMessage::Data(batch)) => {
                    let mut batch = batch.iter();
                    let event = batch.next().expect("Streamed batch should not be empty");
                    assert_eq!(&alloy::sol_types::SolEvent::encode_log_data(expected), event.data(), "Unexpected event: {:#?}\nExpected: {:#?}\nRemaining: {:#?}", event, expected, remaining);
                    while let Some(event) = batch.next() {
                        let expected = remaining.next().unwrap_or_else(|| panic!("Received more events than expected, current: {:#?}\nStreamed batch: {:#?}", event, batch));
                        assert_eq!(&alloy::sol_types::SolEvent::encode_log_data(expected), event.data(), "Unexpected event: {:#?}\nExpected: {:#?}\nRemaining: {:#?}", event, expected, remaining);
                    }
                }
                Some(other) => {
                    panic!("Expected ScannerMessage::Data, got: {:#?}", other);
                }
                None => {panic!("Stream closed while still expecting: {:#?}", remaining);}
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
