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
pub mod server;

mod util;

pub use error::{ConnectionError, Reason};
pub use frame::{StreamId};
pub use proto::Connection;

use bytes::Bytes;

pub type FrameSize = u32;

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
        headers: (),
    },
    PushPromise {
        id: StreamId,
        promise: (),
    },
    Reset {
        id: StreamId,
        error: Reason,
    },
}

/// Either a Client or a Server
pub trait Peer {
    /// Message type sent into the transport
    type Send;

    /// Message type polled from the transport
    type Poll;

    /// Returns `true` if `id` is a valid StreamId for a stream initiated by the
    /// local node.
    fn is_valid_local_stream_id(id: StreamId) -> bool;

    /// Returns `true` if `id` is a valid StreamId for a stream initiated by the
    /// remote node.
    fn is_valid_remote_stream_id(id: StreamId) -> bool;

    fn local_can_open() -> bool;
    fn remote_can_open() -> bool {
        !Self::local_can_open()
    }

    //fn can_reserve_local_stream() -> bool;
    // fn can_reserve_remote_stream() -> bool {
    //     !self.can_reserve_local_stream
    // }

    #[doc(hidden)]
    fn convert_send_message(
        id: StreamId,
        headers: Self::Send,
        end_of_stream: bool) -> frame::Headers;

    #[doc(hidden)]
    fn convert_poll_message(headers: frame::Headers) -> Self::Poll;
}
