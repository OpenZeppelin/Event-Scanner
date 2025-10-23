use alloy::{rpc::types::Log, sol_types::SolEvent};

use crate::{EventScannerError, ScannerMessage};

pub type Message = ScannerMessage<Vec<Log>, EventScannerError>;

impl From<Vec<Log>> for Message {
    fn from(logs: Vec<Log>) -> Self {
        Message::Data(logs)
    }
}

impl<E: SolEvent> PartialEq<Vec<E>> for Message {
    fn eq(&self, other: &Vec<E>) -> bool {
        self.eq(&other.as_slice())
    }
}

impl<E: SolEvent> PartialEq<&Vec<E>> for Message {
    fn eq(&self, other: &&Vec<E>) -> bool {
        self.eq(&other.as_slice())
    }
}

impl<E: SolEvent, const N: usize> PartialEq<&[E; N]> for Message {
    fn eq(&self, other: &&[E; N]) -> bool {
        self.eq(&other.as_slice())
    }
}

impl<E: SolEvent> PartialEq<&[E]> for Message {
    fn eq(&self, other: &&[E]) -> bool {
        if let Message::Data(logs) = self {
            logs.iter().map(|l| l.data().clone()).eq(other.iter().map(SolEvent::encode_log_data))
        } else {
            false
        }
    }
}
