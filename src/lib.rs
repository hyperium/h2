#![allow(warnings)]

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

/// An H2 connection frame
pub enum Frame<T> {
    Message {
        id: StreamId,
        message: T,
        body: bool,
    },
    Body {
        id: StreamId,
        chunk: Option<()>,
    },
    Error {
        id: StreamId,
        error: (),
    }
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
        message: Self::Send,
        body: bool) -> proto::SendMessage;

    #[doc(hidden)]
    fn convert_poll_message(message: proto::PollMessage) -> Frame<Self::Poll>;
}
