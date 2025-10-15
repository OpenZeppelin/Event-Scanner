use alloy::sol_types::SolEvent;

use crate::event_scanner::EventScannerMessage;

#[derive(Debug)]
pub struct LogMetadata<E: SolEvent> {
    pub event: E,
    pub address: alloy::primitives::Address,
    pub tx_hash: alloy::primitives::B256,
}

impl<E: SolEvent> PartialEq<Vec<LogMetadata<E>>> for EventScannerMessage {
    fn eq(&self, other: &Vec<LogMetadata<E>>) -> bool {
        self.eq(&other.as_slice())
    }
}

impl<E: SolEvent> PartialEq<&Vec<LogMetadata<E>>> for EventScannerMessage {
    fn eq(&self, other: &&Vec<LogMetadata<E>>) -> bool {
        self.eq(&other.as_slice())
    }
}

impl<E: SolEvent, const N: usize> PartialEq<&[LogMetadata<E>; N]> for EventScannerMessage {
    fn eq(&self, other: &&[LogMetadata<E>; N]) -> bool {
        self.eq(&other.as_slice())
    }
}

impl<E: SolEvent> PartialEq<&[LogMetadata<E>]> for EventScannerMessage {
    fn eq(&self, other: &&[LogMetadata<E>]) -> bool {
        if let EventScannerMessage::Data(logs) = self {
            let log_data = logs
                .iter()
                .map(|l| {
                    let address = l.address();
                    let tx_hash = l.transaction_hash.unwrap();
                    (l.inner.data.clone(), address, tx_hash)
                })
                .collect::<Vec<_>>();
            let expected = other
                .iter()
                .map(|e| (e.event.encode_log_data(), e.address, e.tx_hash))
                .collect::<Vec<_>>();
            log_data == expected
        } else {
            false
        }
    }
}
