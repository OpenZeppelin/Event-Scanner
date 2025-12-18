#[cfg(feature = "tracing")]
#[allow(unused_macros)]
macro_rules! error {
    ($($arg:tt)*) => {
        tracing::error!($($arg)*)
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
        tracing::warn!($($arg)*)
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
        tracing::info!($($arg)*)
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
        tracing::debug!($($arg)*)
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
        tracing::trace!($($arg)*)
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
