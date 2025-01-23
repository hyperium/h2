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

pub(crate) use debug;

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

pub(crate) use trace;

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

pub(crate) use trace_span;

macro_rules! _warn {
    ($($arg:tt)*) => {
        {
            #[cfg(feature = "tracing")]
            {
                ::tracing::warn!($($arg)+);
            }
        }
    }
}

pub(crate) use _warn as warn;
