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
