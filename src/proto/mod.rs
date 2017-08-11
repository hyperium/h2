mod codec;
mod connection;
mod framed_read;
mod framed_write;
mod ping_pong;
mod settings;
mod streams;

pub use self::connection::Connection;
pub use self::streams::{Streams, StreamRef, Chunk};

use self::codec::Codec;
use self::framed_read::FramedRead;
use self::framed_write::FramedWrite;
use self::ping_pong::PingPong;
use self::settings::Settings;

use {StreamId, ConnectionError};
use error::Reason;
use frame::{self, Frame};

use futures::{self, task, Poll, Async, AsyncSink, Sink, Stream as Stream2};
use bytes::{Buf, IntoBuf};
use tokio_io::{AsyncRead, AsyncWrite};
use tokio_io::codec::length_delimited;

/// Either a Client or a Server
pub trait Peer {
    /// Message type sent into the transport
    type Send;

    /// Message type polled from the transport
    type Poll;

    fn is_server() -> bool;

    #[doc(hidden)]
    fn convert_send_message(
        id: StreamId,
        headers: Self::Send,
        end_of_stream: bool) -> frame::Headers;

    #[doc(hidden)]
    fn convert_poll_message(headers: frame::Headers) -> Result<Self::Poll, ConnectionError>;
}

pub type PingPayload = [u8; 8];

pub type WindowSize = u32;

#[derive(Debug, Copy, Clone)]
pub struct WindowUpdate {
    stream_id: StreamId,
    increment: WindowSize,
}

// Constants
pub const DEFAULT_INITIAL_WINDOW_SIZE: WindowSize = 65_535;
pub const MAX_WINDOW_SIZE: WindowSize = ::std::u32::MAX;

/// Create a transport prepared to handle the server handshake.
///
/// When the server is performing the handshake, it is able to only send
/// `Settings` frames and is expected to receive the client preface as a byte
/// stream. To represent this, `Settings<FramedWrite<T>>` is returned.
pub fn framed_write<T, B>(io: T) -> FramedWrite<T, B>
    where T: AsyncRead + AsyncWrite,
          B: Buf,
{
    FramedWrite::new(io)
}

/// Create a full H2 transport from the server handshaker
pub fn from_framed_write<T, P, B>(framed_write: FramedWrite<T, B::Buf>)
    -> Connection<T, P, B>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
          B: IntoBuf,
{
    // Delimit the frames.
    let framed = length_delimited::Builder::new()
        .big_endian()
        .length_field_length(3)
        .length_adjustment(9)
        .num_skip(0) // Don't skip the header
        // TODO: make this configurable and allow it to be changed during
        // runtime.
        .max_frame_length(frame::DEFAULT_MAX_FRAME_SIZE as usize)
        .new_read(framed_write);

    let codec = Codec::from_framed(FramedRead::new(framed));

    Connection::new(codec)
}

impl WindowUpdate {
    pub fn new(stream_id: StreamId, increment: WindowSize) -> WindowUpdate {
        WindowUpdate {
            stream_id,
            increment
        }
    }

    pub fn stream_id(&self) -> StreamId {
        self.stream_id
    }

    pub fn increment(&self) -> WindowSize {
        self.increment
    }
}
