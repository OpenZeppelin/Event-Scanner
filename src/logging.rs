#[cfg(feature = "tracing")]
macro_rules! opt_error {
    ($($arg:tt)*) => {
        tracing::opt_error!($($arg)*)
    };
}

#[cfg(not(feature = "tracing"))]
macro_rules! opt_error {
    ($($arg:tt)*) => {
        let _ = ($(&stringify!($arg)),*);
    };
}

#[cfg(feature = "tracing")]
macro_rules! opt_warn {
    ($($arg:tt)*) => {
        tracing::warn!($($arg)*)
    };
}

#[cfg(not(feature = "tracing"))]
macro_rules! opt_warn {
    ($($arg:tt)*) => {
        ()
    };
}

#[cfg(feature = "tracing")]
macro_rules! opt_info {
    ($($arg:tt)*) => {
        tracing::info!($($arg)*)
    };
}

#[cfg(not(feature = "tracing"))]
macro_rules! opt_info {
    ($($arg:tt)*) => {
        ()
    };
}

#[cfg(feature = "tracing")]
macro_rules! opt_debug {
    ($($arg:tt)*) => {
        tracing::debug!($($arg)*)
    };
}

#[cfg(not(feature = "tracing"))]
macro_rules! opt_debug {
    ($($arg:tt)*) => {
        ()
    };
}

#[cfg(feature = "tracing")]
macro_rules! opt_trace {
    ($($arg:tt)*) => {
        tracing::trace!($($arg)*)
    };
}

#[cfg(not(feature = "tracing"))]
macro_rules! opt_trace {
    ($($arg:tt)*) => {
        ()
    };
}

pub(crate) use opt_debug;
pub(crate) use opt_error;
pub(crate) use opt_info;
pub(crate) use opt_trace;
pub(crate) use opt_warn;
