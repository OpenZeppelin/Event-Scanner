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
macro_rules! assert_next_any {
    ($stream: expr, $expected_options: expr) => {
        assert_next_any!($stream, $expected_options, timeout = 5)
    };
    ($stream: expr, $expected_options: expr, timeout = $secs: expr) => {
        let message = tokio::time::timeout(
            std::time::Duration::from_secs($secs),
            tokio_stream::StreamExt::next(&mut $stream),
        )
        .await
        .expect("timed out");

        if let Some(data) = message {
            let matched = $expected_options.iter().any(|expected| data == *expected);
            assert!(matched, "Expected one of:\n{:#?}\n\nGot:\n{:#?}", $expected_options, data);
        } else {
            panic!("Expected one of {:?}, but channel was closed", $expected_options)
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
