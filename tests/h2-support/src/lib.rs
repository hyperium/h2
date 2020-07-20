//! Utilities to support tests.

#[macro_use]
pub mod assert;

pub mod raw;

pub mod frames;
pub mod mock;
pub mod prelude;
pub mod util;

mod client_ext;
mod future_ext;

pub use crate::client_ext::SendRequestExt;
pub use crate::future_ext::TestFuture;

pub type WindowSize = usize;
pub const DEFAULT_WINDOW_SIZE: WindowSize = (1 << 16) - 1;

// This is our test Codec type
pub type Codec<T> = h2::Codec<T, bytes::Bytes>;

// This is the frame type that is sent
pub type SendFrame = h2::frame::Frame<bytes::Bytes>;

#[macro_export]
macro_rules! trace_init {
    () => {
        let subscriber = $crate::prelude::tracing_subscriber::fmt()
            .with_max_level($crate::prelude::tracing::Level::TRACE)
            .with_span_events($crate::prelude::tracing_subscriber::fmt::format::FmtSpan::CLOSE)
            .finish();
        let _guard = $crate::prelude::tracing::subscriber::set_default(subscriber);
        let span = $crate::prelude::tracing::info_span!(
            "test", 
            "{}", 
            // get the name of the test thread to generate a unique span for the test
            std::thread::current().name().expect("test threads must be named")
        );
        let _e = span.enter();
    }
}
