use hpack;
use error::{ConnectionError, Reason};

use bytes::Bytes;

use std::fmt;

/// A helper macro that unpacks a sequence of 4 bytes found in the buffer with
/// the given identifier, starting at the given offset, into the given integer
/// type. Obviously, the integer type should be able to support at least 4
/// bytes.
///
/// # Examples
///
/// ```rust
/// let buf: [u8; 4] = [0, 0, 0, 1];
/// assert_eq!(1u32, unpack_octets_4!(buf, 0, u32));
/// ```
#[macro_escape]
macro_rules! unpack_octets_4 {
    // TODO: Get rid of this macro
    ($buf:expr, $offset:expr, $tip:ty) => (
        (($buf[$offset + 0] as $tip) << 24) |
        (($buf[$offset + 1] as $tip) << 16) |
        (($buf[$offset + 2] as $tip) <<  8) |
        (($buf[$offset + 3] as $tip) <<  0)
    );
}

mod data;
mod go_away;
mod head;
mod headers;
mod ping;
mod reset;
mod settings;
mod stream_id;
mod util;
mod window_update;

pub use self::data::Data;
pub use self::go_away::GoAway;
pub use self::head::{Head, Kind};
pub use self::headers::{Headers, PushPromise, Continuation, Pseudo};
pub use self::ping::Ping;
pub use self::reset::Reset;
pub use self::settings::{Settings, SettingSet};
pub use self::stream_id::StreamId;
pub use self::window_update::WindowUpdate;

// Re-export some constants
pub use self::settings::{
    DEFAULT_SETTINGS_HEADER_TABLE_SIZE,
    DEFAULT_MAX_FRAME_SIZE,
};

pub const HEADER_LEN: usize = 9;

pub enum Frame<T = Bytes> {
    Data(Data<T>),
    Headers(Headers),
    PushPromise(PushPromise),
    Settings(Settings),
    Ping(Ping),
    WindowUpdate(WindowUpdate),
    Reset(Reset)
}

impl<T> Frame<T> {
    /// Returns true if the frame is a DATA frame.
    pub fn is_data(&self) -> bool {
        use self::Frame::*;

        match *self {
            Data(..) => true,
            _ => false,
        }
    }
}

impl<T> fmt::Debug for Frame<T> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        use self::Frame::*;

        match *self {
            Data(..) => write!(fmt, "Frame::Data(..)"),
            Headers(ref frame) => write!(fmt, "Frame::Headers({:?})", frame),
            PushPromise(ref frame) => write!(fmt, "Frame::PushPromise({:?})", frame),
            Settings(ref frame) => write!(fmt, "Frame::Settings({:?})", frame),
            Ping(ref frame) => write!(fmt, "Frame::Ping({:?})", frame),
            WindowUpdate(ref frame) => write!(fmt, "Frame::WindowUpdate({:?})", frame),
            Reset(ref frame) => write!(fmt, "Frame::Reset({:?})", frame),
        }
    }
}

/// Errors that can occur during parsing an HTTP/2 frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A full frame header was not passed.
    Short,

    /// An unsupported value was set for the flag value.
    BadFlag,

    /// An unsupported value was set for the frame kind.
    BadKind,

    /// A length value other than 8 was set on a PING message.
    BadFrameSize,

    /// The padding length was larger than the frame-header-specified
    /// length of the payload.
    TooMuchPadding,

    /// The payload length specified by the frame header was shorter than
    /// necessary for the parser settings specified and the frame type.
    ///
    /// This happens if, for instance, the priority flag is set and the
    /// header length is shorter than a stream dependency.
    ///
    /// `PayloadLengthTooShort` should be treated as a protocol error.
    PayloadLengthTooShort,

    /// The payload length specified by the frame header of a settings frame
    /// was not a round multiple of the size of a single setting.
    PartialSettingLength,

    /// The payload length specified by the frame header was not the
    /// value necessary for the specific frame type.
    InvalidPayloadLength,

    /// Received a payload with an ACK settings frame
    InvalidPayloadAckSettings,

    /// An invalid stream identifier was provided.
    ///
    /// This is returned if a SETTINGS or PING frame is received with a stream
    /// identifier other than zero.
    InvalidStreamId,

    /// Failed to perform HPACK decoding
    Hpack(hpack::DecoderError),
}

// ===== impl Error =====

impl From<Error> for ConnectionError {
    fn from(src: Error) -> ConnectionError {
        match src {
            // TODO: implement
            _ => ConnectionError::Proto(Reason::ProtocolError),
        }
    }
}
