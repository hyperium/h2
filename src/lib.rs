#![allow(warnings)]
// #![deny(missing_debug_implementations)]

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
pub use frame::StreamId;

use bytes::Bytes;

pub type FrameSize = u32;
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

impl<B: IntoBuf> futures::Stream for Body<B> {
    type Item = Bytes;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        self.inner.poll_data()
    }
}

// TODO: Delete below

/// An H2 connection frame
#[derive(Debug)]
pub enum Frame<T, B = Bytes> {
    Headers {
        id: StreamId,
        headers: T,
        end_of_stream: bool,
    },
    Data {
        id: StreamId,
        data: B,
        end_of_stream: bool,
    },
    Trailers {
        id: StreamId,
        headers: HeaderMap,
    },
    PushPromise {
        id: StreamId,
        promised_id: StreamId,
    },
    Reset {
        id: StreamId,
        error: Reason,
    },
}
