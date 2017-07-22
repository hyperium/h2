use hpack;
use error::{ConnectionError, Reason};

use bytes::Bytes;

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

#[derive(Debug /*, Clone, PartialEq */)]
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
    pub fn is_connection_frame(&self) -> bool {
        use self::Frame::*;

        match self {
            &Headers(..) |
            &Data(..) |
            &PushPromise(..) |
            &Reset(..) => false,

            &WindowUpdate(ref v) => v.stream_id().is_zero(),

            &Ping(_) |
            &Settings(_) => true,
        }
    }

    pub fn stream_id(&self) -> StreamId {
        use self::Frame::*;

        match self {
            &Headers(ref v) => v.stream_id(),
            &Data(ref v) => v.stream_id(),
            &PushPromise(ref v) => v.stream_id(),
            &WindowUpdate(ref v) => v.stream_id(),
            &Reset(ref v) => v.stream_id(),

            &Ping(_) |
            &Settings(_) => StreamId::zero(),
        }
    }

    pub fn is_end_stream(&self) -> bool {
        use self::Frame::*;

        match self {
            &Headers(ref v) => v.is_end_stream(),
            &Data(ref v) => v.is_end_stream(),

            &PushPromise(_) |
            &WindowUpdate(_) |
            &Ping(_) |
            &Settings(_) => false,

            &Reset(_) => true,
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
