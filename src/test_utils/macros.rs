#[macro_export]
macro_rules! assert_next {
    ($stream: expr, $expected: expr) => {
        let message = tokio::time::timeout(
            std::time::Duration::from_secs(5),
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
        let message = tokio::time::timeout(
            std::time::Duration::from_secs(5),
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
