#[macro_export]
macro_rules! assert_next_logs {
    ($stream: expr, $expected: expr) => {
        let message = tokio_stream::StreamExt::next(&mut $stream).await;
        if let Some(event_scanner::event_scanner::EventScannerMessage::Data(logs)) = message {
            let log_data = logs.into_iter().map(|l| l.inner.data).collect::<Vec<_>>();
            let expected = $expected.iter().map(|e| e.encode_log_data()).collect::<Vec<_>>();
            assert_eq!(log_data, expected);
        } else {
            panic!("Expected EventScannerMessage::Data, got: {message:?}");
        }
    };
}

#[macro_export]
macro_rules! assert_next_status {
    ($stream: expr, $expected: expr) => {
        let message = tokio_stream::StreamExt::next(&mut $stream).await;
        if let Some(event_scanner::event_scanner::EventScannerMessage::Status(status)) = message {
            assert_eq!(status, $expected);
        } else {
            panic!("Expected EventScannerMessage::Status, got: {message:?}");
        }
    };
}
