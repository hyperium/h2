// #![allow(warnings)]
#![deny(missing_debug_implementations)]

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

extern crate ordermap;

extern crate byteorder;

#[macro_use]
extern crate log;

pub mod client;
pub mod error;
pub mod hpack;
pub mod proto;
pub mod frame;

mod util;

pub use error::{ConnectionError, StreamError, Reason};
pub use frame::{StreamId};
pub use proto::Connection;

use bytes::Bytes;

/// An H2 connection frame
#[derive(Debug)]
pub enum Frame<T> {
    Headers {
        id: StreamId,
        headers: T,
        end_of_stream: bool,
    },
    Body {
        id: StreamId,
        chunk: Bytes,
        end_of_stream: bool,
    },
    Trailers {
        id: StreamId,
        headers: (),
    },
    PushPromise {
        id: StreamId,
        promise: (),
    },
    Error {
        id: StreamId,
        error: (),
    },
}

/// Either a Client or a Server
pub trait Peer {
    /// Message type sent into the transport
    type Send;

    /// Message type polled from the transport
    type Poll;

    fn check_initiating_id(id: StreamId) -> Result<(), ConnectionError>;

    #[doc(hidden)]
    fn convert_send_message(
        id: StreamId,
        headers: Self::Send,
        end_of_stream: bool) -> frame::Headers;

    #[doc(hidden)]
    fn convert_poll_message(headers: frame::Headers) -> Self::Poll;
}
