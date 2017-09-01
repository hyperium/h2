// #![deny(warnings, missing_debug_implementations)]

#[macro_use]
extern crate futures;

#[macro_use]
extern crate tokio_io;

// HTTP types
extern crate http;

// Buffer utilities
extern crate bytes;

// Hash function used for HPACK encoding and tracking stream states.
extern crate fnv;

extern crate byteorder;

extern crate slab;

#[macro_use]
extern crate log;

extern crate string;

mod error;
mod codec;
mod hpack;
mod proto;
mod frame;

pub mod client;
pub mod server;

pub use error::{Error, Reason};
