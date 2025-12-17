#[cfg(feature = "tracing")]
macro_rules! opt_error {
    ($($arg:tt)*) => {
        tracing::error!($($arg)*)
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
