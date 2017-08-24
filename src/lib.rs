#![deny(warnings, missing_debug_implementations)]

#[macro_use]
extern crate futures;

#[macro_use]
extern crate tokio_io;

extern crate tokio_timer;

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

pub mod client;
pub mod error;
mod hpack;
mod proto;
mod frame;
pub mod server;

pub use error::{ConnectionError, Reason};

use bytes::Bytes;

// TODO: remove if carllerche/http#90 lands
pub type HeaderMap = http::HeaderMap<http::header::HeaderValue>;

// TODO: Move into other location

use bytes::IntoBuf;
use futures::Poll;

#[derive(Debug)]
pub struct Body<B: IntoBuf> {
    inner: proto::StreamRef<B::Buf>,
}

// ===== impl Body =====

impl<B: IntoBuf> Body<B> {
    pub fn is_empty(&self) -> bool {
        // If the recv side is closed and the receive queue is empty, the body is empty.
        self.inner.body_is_empty()
    }

    pub fn release_capacity(&mut self, sz: usize) -> Result<(), ConnectionError> {
        self.inner.release_capacity(sz as proto::WindowSize)
    }

    /// Poll trailers
    ///
    /// This function **must** not be called until `Body::poll` returns `None`.
    pub fn poll_trailers(&mut self) -> Poll<Option<HeaderMap>, ConnectionError> {
        self.inner.poll_trailers()
    }
}

impl<B: IntoBuf> futures::Stream for Body<B> {
    type Item = Bytes;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        self.inner.poll_data()
    }
}
