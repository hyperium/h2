macro_rules! debug {
    ($($arg:tt)+) => {
        {
            #[cfg(feature = "tracing")]
            {
                ::tracing::debug!($($arg)+);
            }
        }
    }
}

macro_rules! trace {
    ($($arg:tt)*) => {
        {
            #[cfg(feature = "tracing")]
            {
                ::tracing::trace!($($arg)+);
            }
        }
    }
}

macro_rules! trace_span {
    ($($arg:tt)*) => {
        {
            #[cfg(feature = "tracing")]
            {
                let _span = ::tracing::trace_span!($($arg)+);
                _span.entered()
            }
        }
    }
}

macro_rules! warn {
    ($($arg:tt)*) => {
        {
            #[cfg(feature = "tracing")]
            {
                ::tracing::warn!($($arg)+);
            }
        }
    }
}
