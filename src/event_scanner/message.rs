use alloy::{rpc::types::Log, sol_types::SolEvent};

use crate::{EventScannerError, ScannerMessage};

pub type EventScannerMessage = ScannerMessage<Vec<Log>, EventScannerError>;

impl From<Vec<Log>> for EventScannerMessage {
    fn from(logs: Vec<Log>) -> Self {
        EventScannerMessage::Data(logs)
    }
}

impl<E: SolEvent> PartialEq<Vec<E>> for EventScannerMessage {
    fn eq(&self, other: &Vec<E>) -> bool {
        self.eq(&other.as_slice())
    }
}

impl<E: SolEvent> PartialEq<&Vec<E>> for EventScannerMessage {
    fn eq(&self, other: &&Vec<E>) -> bool {
        self.eq(&other.as_slice())
    }
}

impl<E: SolEvent, const N: usize> PartialEq<&[E; N]> for EventScannerMessage {
    fn eq(&self, other: &&[E; N]) -> bool {
        self.eq(&other.as_slice())
    }
}

impl<E: SolEvent> PartialEq<&[E]> for EventScannerMessage {
    fn eq(&self, other: &&[E]) -> bool {
        if let EventScannerMessage::Data(logs) = self {
            logs.iter().map(|l| l.data().clone()).eq(other.iter().map(SolEvent::encode_log_data))
        } else {
            false
        }
    }
}
