//! Utilities to support tests.

#[cfg(not(feature = "unstable"))]
compile_error!(
    "Tests depend on the 'unstable' feature on h2. \
    Retry with `cargo test --features unstable`"
);

pub extern crate bytes;
pub extern crate env_logger;
pub extern crate futures;
pub extern crate h2;
pub extern crate http;
pub extern crate mock_io;
pub extern crate tokio_io;

// Kind of annoying, but we can't use macros from crates that aren't specified
// at the root.
macro_rules! try_ready {
    ($e:expr) => ({
        use $crate::support::futures::Async;
        match $e {
            Ok(Async::Ready(t)) => t,
            Ok(Async::NotReady) => return Ok(Async::NotReady),
            Err(e) => return Err(From::from(e)),
        }
    })
}

macro_rules! try_nb {
    ($e:expr) => ({
        use $crate::support::futures::Async;
        use ::std::io::ErrorKind::WouldBlock;

        match $e {
            Ok(t) => t,
            Err(ref e) if e.kind() == WouldBlock => {
                return Ok(Async::NotReady)
            }
            Err(e) => return Err(e.into()),
        }
    })
}

#[macro_use]
mod assert;

#[macro_use]
pub mod raw;

pub mod frames;
pub mod prelude;
pub mod mock;
pub mod util;

mod future_ext;

pub use self::future_ext::{FutureExt, Unwrap};

// This is our test Codec type
pub type Codec<T> = h2::Codec<T, ::std::io::Cursor<::bytes::Bytes>>;

// This is the frame type that is sent
pub type SendFrame = h2::frame::Frame<::std::io::Cursor<::bytes::Bytes>>;
