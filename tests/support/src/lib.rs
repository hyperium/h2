//! Utilities to support tests.

pub extern crate bytes;
pub extern crate h2;
pub extern crate http;

#[macro_use]
pub extern crate tokio_io;

pub extern crate env_logger;
#[macro_use]
pub extern crate futures;
pub extern crate mock_io;

#[macro_use]
mod assert;

#[macro_use]
pub mod raw;

pub mod frames;
pub mod prelude;
pub mod mock;
pub mod util;

mod future_ext;

pub use future_ext::{FutureExt, Unwrap};

// This is our test Codec type
pub type Codec<T> = h2::Codec<T, ::std::io::Cursor<::bytes::Bytes>>;

// This is the frame type that is sent
pub type SendFrame = h2::frame::Frame<::std::io::Cursor<::bytes::Bytes>>;
