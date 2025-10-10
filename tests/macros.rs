use alloy::{
    primitives::{Address, B256},
    sol_types::SolEvent,
};

#[macro_export]
macro_rules! assert_next_events {
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

pub struct LogMetadata<E: SolEvent> {
    pub event: E,
    pub address: Address,
    pub tx_hash: B256,
}

#[macro_export]
macro_rules! assert_next_logs {
    ($stream: expr, $expected: expr) => {
        let message = tokio_stream::StreamExt::next(&mut $stream).await;
        if let Some(event_scanner::event_scanner::EventScannerMessage::Data(logs)) = message {
            let log_data = logs
                .into_iter()
                .map(|l| {
                    let address = l.address();
                    let tx_hash = l.transaction_hash.unwrap();
                    (l.inner.data, address, tx_hash)
                })
                .collect::<Vec<_>>();
            let expected = $expected
                .iter()
                .map(|e| (e.event.encode_log_data(), e.address, e.tx_hash))
                .collect::<Vec<_>>();
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
