//! An asynchronous, HTTP/2.0 server and client implementation.
//!
//! This library implements the [HTTP/2.0] specification. The implementation is
//! asynchronous, using [futures] as the basis for the API. The implementation
//! is also decoupled from TCP or TLS details. The user must handle ALPN and
//! HTTP/1.1 upgrades themselves.
//!
//! # Getting started
//!
//! Add the following to your `Cargo.toml` file:
//!
//! ```toml
//! [dependencies]
//! h2 = { git = 'https://github.com/carllerche/h2' } # soon to be on crates.io!
//! ```
//!
//! Next, add this to your crate:
//!
//! ```no_run
//! extern crate h2;
//! ```
//!
//! # Layout
//!
//! The crate is split into [`client`] and [`server`] modules. Types that are
//! common to both clients and servers are located at the root of the crate. See
//! module level documentation for more details on how to use `h2`.
//!
//! # Handshake
//!
//! TODO: Discuss how to initialize HTTP/2.0 clients and servers.
//!
//! # Outbound data type
//!
//! TODO: Discuss how to customize the outbound data type
//!
//! # Flow control
//!
//! [Flow control] is a fundamental feature of HTTP/2.0. The `h2` library
//! exposes flow control to the user. Be sure to read the flow control
//! documentation in order to avoid introducing potential dead locking in your
//! code.
//!
//! Managing flow control for inbound data is done through [`ReleaseCapacity`].
//! Managing flow control for outbound data is done through [`SendStream`]. See
//! the struct level documentation for those two types for more details.
//!
//! [HTTP/2.0]: https://http2.github.io/
//! [futures]: https://docs.rs/futures/
//! [`client`]: client/index.html
//! [`server`]: server/index.html
//! [Flow control]: http://httpwg.org/specs/rfc7540.html#FlowControl
//! [`ReleaseCapacity`]: struct.ReleaseCapacity.html
//! [`SendStream`]: struct.SendStream.html

#![deny(warnings, missing_debug_implementations, missing_docs)]

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
extern crate ordermap;

mod error;
#[cfg_attr(feature = "unstable", allow(missing_docs))]
mod codec;
mod hpack;
mod proto;

#[cfg(not(feature = "unstable"))]
mod frame;

#[cfg(feature = "unstable")]
#[allow(missing_docs)]
pub mod frame;

pub mod client;
pub mod server;
mod share;

pub use error::{Error, Reason};
pub use share::{SendStream, RecvStream, ReleaseCapacity};

#[cfg(feature = "unstable")]
pub use codec::{Codec, RecvError, SendError, UserError};
