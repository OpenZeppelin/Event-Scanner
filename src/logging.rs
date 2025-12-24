//! Internal logging macros that wrap `tracing` when the feature is enabled.
//!
//! When the `tracing` feature is disabled, all logging calls compile to no-ops,
//! ensuring zero runtime cost for users who don't need observability.

#[cfg(feature = "tracing")]
#[allow(unused_macros)]
macro_rules! error {
    ($($arg:tt)*) => {
        tracing::error!(target: "event_scanner", $($arg)*)
    };
}

#[cfg(not(feature = "tracing"))]
#[allow(unused_macros)]
macro_rules! error {
    ($($arg:tt)*) => {
        $crate::__trace_consume!($($arg)*)
    };
}

#[cfg(feature = "tracing")]
#[allow(unused_macros)]
macro_rules! warn {
    ($($arg:tt)*) => {
        tracing::warn!(target: "event_scanner", $($arg)*)
    };
}

#[cfg(not(feature = "tracing"))]
#[allow(unused_macros)]
macro_rules! warn {
    ($($arg:tt)*) => {
        $crate::__trace_consume!($($arg)*)
    };
}

#[cfg(feature = "tracing")]
#[allow(unused_macros)]
macro_rules! info {
    ($($arg:tt)*) => {
        tracing::info!(target: "event_scanner", $($arg)*)
    };
}

#[cfg(not(feature = "tracing"))]
#[allow(unused_macros)]
macro_rules! info {
    ($($arg:tt)*) => {
        $crate::__trace_consume!($($arg)*)
    };
}

#[cfg(feature = "tracing")]
#[allow(unused_macros)]
macro_rules! debug {
    ($($arg:tt)*) => {
        tracing::debug!(target: "event_scanner", $($arg)*)
    };
}

#[cfg(not(feature = "tracing"))]
#[allow(unused_macros)]
macro_rules! debug {
    ($($arg:tt)*) => {
        $crate::__trace_consume!($($arg)*)
    };
}

#[cfg(feature = "tracing")]
#[allow(unused_macros)]
macro_rules! trace {
    ($($arg:tt)*) => {
        tracing::trace!(target: "event_scanner", $($arg)*)
    };
}

#[cfg(not(feature = "tracing"))]
#[allow(unused_macros)]
macro_rules! trace {
    ($($arg:tt)*) => {
        $crate::__trace_consume!($($arg)*)
    };
}

#[doc(hidden)]
#[macro_export]
#[cfg(not(feature = "tracing"))]
#[allow(unused_macros)]
macro_rules! __trace_consume {
    // field = %expr, rest...
    ($field:ident = % $value:expr, $($rest:tt)*) => {
        { let _ = &$value; $crate::__trace_consume!($($rest)*); }
    };
    // field = ?expr, rest...
    ($field:ident = ? $value:expr, $($rest:tt)*) => {
        { let _ = &$value; $crate::__trace_consume!($($rest)*); }
    };
    // field = expr, rest...
    ($field:ident = $value:expr, $($rest:tt)*) => {
        { let _ = &$value; $crate::__trace_consume!($($rest)*); }
    };
    // String literal or other tokens - ignore
    ($lit:literal $($rest:tt)*) => {
        $crate::__trace_consume!($($rest)*)
    };
    // Base case - empty
    () => {};
}
