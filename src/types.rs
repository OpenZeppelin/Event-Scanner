use std::error::Error;

#[derive(Copy, Debug, Clone)]
pub enum ScannerMessage<T: Clone, E: Error + Clone> {
    Message(T),
    Error(E),
    Info(ScannerInfo),
}

#[derive(Copy, Debug, Clone)]
pub enum ScannerInfo {
    ChainTipReached,
    HistoricalSyncCompleted,
    ReorgHandled,
}
