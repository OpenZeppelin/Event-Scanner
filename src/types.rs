use std::error::Error;

#[derive(Copy, Debug, Clone)]
pub enum ScannerMessage<T: Clone, E: Error + Clone> {
    Data(T),
    Error(E),
    Status(ScannerStatus),
}

#[derive(Copy, Debug, Clone, PartialEq)]
pub enum ScannerStatus {
    ChainTipReached,
    ReorgDetected,
}

impl<T: Clone, E: Error + Clone> From<ScannerStatus> for ScannerMessage<T, E> {
    fn from(value: ScannerStatus) -> Self {
        ScannerMessage::Status(value)
    }
}

impl<T: Clone, E: Error + Clone> PartialEq<ScannerStatus> for ScannerMessage<T, E> {
    fn eq(&self, other: &ScannerStatus) -> bool {
        if let ScannerMessage::Status(status) = self { status == other } else { false }
    }
}
