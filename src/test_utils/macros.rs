#[macro_export]
macro_rules! assert_next {
    ($stream: expr, None) => {
        let message = tokio_stream::StreamExt::next(&mut $stream).await;
        assert!(message.is_none())
    };
    ($stream: expr, $expected: expr) => {
        let message = tokio_stream::StreamExt::next(&mut $stream).await;
        if let Some(msg) = message {
            assert_eq!(msg, $expected)
        } else {
            panic!("Expected {:?}, got: {message:?}", $expected)
        }
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
