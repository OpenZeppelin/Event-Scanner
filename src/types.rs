use std::error::Error;

#[derive(Copy, Debug, Clone)]
pub enum ScannerMessage<T: Clone, E: Error + Clone> {
    Data(T),
    Error(E),
    Status(ScannerStatus),
}

#[derive(Copy, Debug, Clone)]
pub enum ScannerStatus {
    ChainTipReached,
    ReorgDetected,
}
