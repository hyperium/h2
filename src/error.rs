use std::{error, fmt, io};

/// The error type for HTTP/2 operations
#[derive(Debug)]
pub enum ConnectionError {
    /// The HTTP/2 stream was reset
    Proto(Reason),
    /// An `io::Error` occurred while trying to read or write.
    Io(io::Error),
}

#[derive(Debug)]
pub struct StreamError(Reason);

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Reason {
    NoError,
    ProtocolError,
    InternalError,
    FlowControlError,
    SettingsTimeout,
    StreamClosed,
    FrameSizeError,
    RefusedStream,
    Cancel,
    CompressionError,
    ConnectError,
    EnhanceYourCalm,
    InadequateSecurity,
    Http11Required,
    Other(u32),
}

macro_rules! reason_desc {
    ($reason:expr) => (reason_desc!($reason, ""));
    ($reason:expr, $prefix:expr) => ({
        match $reason {
            Reason::NoError => concat!($prefix, "not a result of an error"),
            Reason::ProtocolError => concat!($prefix, "unspecific protocol error detected"),
            Reason::InternalError => concat!($prefix, "unexpected internal error encountered"),
            Reason::FlowControlError => concat!($prefix, "flow-control protocol violated"),
            Reason::SettingsTimeout => concat!($prefix, "settings ACK not received in timely manner"),
            Reason::StreamClosed => concat!($prefix, "received frame when stream half-closed"),
            Reason::FrameSizeError => concat!($prefix, "frame sent with invalid size"),
            Reason::RefusedStream => concat!($prefix, "refused stream before processing any application logic"),
            Reason::Cancel => concat!($prefix, "stream no longer needed"),
            Reason::CompressionError => concat!($prefix, "unable to maintain the header compression context"),
            Reason::ConnectError => concat!($prefix, "connection established in response to a CONNECT request was reset or abnormally closed"),
            Reason::EnhanceYourCalm => concat!($prefix, "detected excessive load generating behavior"),
            Reason::InadequateSecurity => concat!($prefix, "security properties do not meet minimum requirements"),
            Reason::Http11Required => concat!($prefix, "endpoint requires HTTP/1.1"),
            Reason::Other(_) => concat!($prefix, "other reason"),
        }
    });
}

// ===== impl ConnectionError =====

impl From<io::Error> for ConnectionError {
    fn from(src: io::Error) -> ConnectionError {
        ConnectionError::Io(src)
    }
}

impl From<Reason> for ConnectionError {
    fn from(src: Reason) -> ConnectionError {
        ConnectionError::Proto(src)
    }
}

impl From<ConnectionError> for io::Error {
    fn from(src: ConnectionError) -> io::Error {
        io::Error::new(io::ErrorKind::Other, src)
    }
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        use self::ConnectionError::*;

        match *self {
            Proto(reason) => write!(fmt, "protocol error: {}", reason),
            Io(ref e) => fmt::Display::fmt(e, fmt),
        }
    }
}

impl error::Error for ConnectionError {
    fn description(&self) -> &str {
        use self::ConnectionError::*;

        match *self {
            Io(ref e) => error::Error::description(e),
            Proto(reason) => reason_desc!(reason, "protocol error: "),
        }
    }
}

// ===== impl Reason =====

impl Reason {
    pub fn description(&self) -> &str {
        reason_desc!(*self)
    }
}

impl From<u32> for Reason {
    fn from(src: u32) -> Reason {
        use self::Reason::*;

        match src {
            0x0 => NoError,
            0x1 => ProtocolError,
            0x2 => InternalError,
            0x3 => FlowControlError,
            0x4 => SettingsTimeout,
            0x5 => StreamClosed,
            0x6 => FrameSizeError,
            0x7 => RefusedStream,
            0x8 => Cancel,
            0x9 => CompressionError,
            0xa => ConnectError,
            0xb => EnhanceYourCalm,
            0xc => InadequateSecurity,
            0xd => Http11Required,
            _ => Other(src),
        }
    }
}

impl From<Reason> for u32 {
    fn from(src: Reason) -> u32 {
        use self::Reason::*;

        match src {
            NoError => 0x0,
            ProtocolError => 0x1,
            InternalError => 0x2,
            FlowControlError => 0x3,
            SettingsTimeout => 0x4,
            StreamClosed => 0x5,
            FrameSizeError => 0x6,
            RefusedStream => 0x7,
            Cancel => 0x8,
            CompressionError => 0x9,
            ConnectError => 0xa,
            EnhanceYourCalm => 0xb,
            InadequateSecurity => 0xc,
            Http11Required => 0xd,
            Other(v) => v,
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "{}", self.description())
    }
}
