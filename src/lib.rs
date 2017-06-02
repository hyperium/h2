#![allow(warnings)]

extern crate futures;

#[macro_use]
extern crate tokio_io;

extern crate tokio_timer;

// HTTP types
extern crate http;

// Buffer utilities
extern crate bytes;

// Hash function used for HPACK encoding
extern crate fnv;

#[macro_use]
extern crate log;

pub mod error;
pub mod hpack;
pub mod proto;
pub mod frame;

mod util;

pub use error::{ConnectionError, StreamError, Reason};
pub use proto::Connection;
