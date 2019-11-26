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
