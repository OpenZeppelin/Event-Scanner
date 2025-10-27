use alloy::{rpc::types::Log, sol_types::SolEvent};

use crate::{ScannerError, ScannerMessage, safe_provider::SafeProviderError};

pub type Message = ScannerMessage<Vec<Log>, ScannerError>;

impl From<Vec<Log>> for Message {
    fn from(logs: Vec<Log>) -> Self {
        Message::Data(logs)
    }
}

impl From<SafeProviderError> for Message {
    fn from(error: SafeProviderError) -> Message {
        let scanner_error: ScannerError = error.into();
        scanner_error.into()
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
