use error::{ConnectionError, Reason};
use bytes::{Bytes, BytesMut, BufMut};

use std::io;

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
    ($buf:expr, $offset:expr, $tip:ty) => (
        (($buf[$offset + 0] as $tip) << 24) |
        (($buf[$offset + 1] as $tip) << 16) |
        (($buf[$offset + 2] as $tip) <<  8) |
        (($buf[$offset + 3] as $tip) <<  0)
    );
}

mod data;
mod head;
mod settings;
mod unknown;
mod util;

pub use self::data::Data;
pub use self::head::{Head, Kind, StreamId};
pub use self::settings::{Settings, SettingSet};
pub use self::unknown::Unknown;

const FRAME_HEADER_LEN: usize = 9;

#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /*
    Data(DataFrame<'a>),
    HeadersFrame(HeadersFrame<'a>),
    RstStreamFrame(RstStreamFrame),
    SettingsFrame(SettingsFrame),
    PingFrame(PingFrame),
    GoawayFrame(GoawayFrame<'a>),
    WindowUpdateFrame(WindowUpdateFrame),
    UnknownFrame(RawFrame<'a>),
    */
    Settings(Settings),
    Unknown(Unknown),
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
    /// This is returned if a settings frame is received with a stream
    /// identifier other than zero.
    InvalidStreamId,
}

// ===== impl Frame ======

impl Frame {
    pub fn load(mut frame: Bytes) -> Result<Frame, Error> {
        let head = Head::parse(&frame);

        // Extract the payload from the frame
        let _ = frame.drain_to(FRAME_HEADER_LEN);


        match head.kind() {
            Kind::Unknown => {
                let unknown = Unknown::new(head, frame);
                Ok(Frame::Unknown(unknown))
            }
            _ => unimplemented!(),
        }
    }

    pub fn encode_len(&self) -> usize {
        use self::Frame::*;

        match *self {
            Settings(ref frame) => frame.encode_len(),
            Unknown(ref frame) => frame.encode_len(),
        }
    }

    pub fn encode(&self, dst: &mut BytesMut) -> Result<(), Error> {
        use self::Frame::*;

        debug_assert!(dst.remaining_mut() >= self.encode_len());

        match *self {
            Settings(ref frame) => frame.encode(dst),
            Unknown(ref frame) => frame.encode(dst),
        }
    }
}

// ===== impl Error =====

impl From<Error> for ConnectionError {
    fn from(src: Error) -> ConnectionError {
        use self::Error::*;

        match src {
            // TODO: implement
            _ => ConnectionError::Proto(Reason::ProtocolError),
        }
    }
}
