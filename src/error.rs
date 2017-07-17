use std::{error, fmt, io};

/// The error type for HTTP/2 operations
///
/// XXX does this sufficiently destinguish stream-level errors from connection-level errors?
#[derive(Debug)]
pub enum ConnectionError {
    /// An error caused by an action taken by the remote peer.
    ///
    /// This is either an error received by the peer or caused by an invalid
    /// action taken by the peer (i.e. a protocol error).
    Proto(Reason),

    /// An `io::Error` occurred while trying to read or write.
    Io(io::Error),

    /// An error resulting from an invalid action taken by the user of this
    /// library.
    User(User),

    // TODO: reserve additional variants
}

#[derive(Debug)]
pub struct StreamError(Reason);

impl StreamError {
   pub fn new(r: Reason) -> StreamError {
       StreamError(r)
   }

   pub fn reason(&self) -> Reason {
       self.0
   }
}

impl From<Reason> for StreamError {
    fn from(r: Reason) -> Self {
        StreamError(r)
    }
}

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
    // TODO: reserve additional variants
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum User {
    /// The specified stream ID is invalid.
    ///
    /// For example, using a stream ID reserved for a push promise from the
    /// client or using a non-zero stream ID for settings.
    InvalidStreamId,

    /// The stream ID is no longer accepting frames.
    InactiveStreamId,

    /// The stream is not currently expecting a frame of this type.
    UnexpectedFrameType,

    /// The connection or stream does not have a sufficient flow control window to
    /// transmit a Data frame to the remote.
    FlowControlViolation,

    /// The connection state is corrupt and the connection should be dropped.
    Corrupt,

    /// The stream state has been reset.
    StreamReset,

    /// The application attempted to initiate too many streams to remote.
    MaxConcurrencyExceeded,

    // TODO: reserve additional variants
}

macro_rules! reason_desc {
    ($reason:expr) => (reason_desc!($reason, ""));
    ($reason:expr, $prefix:expr) => ({
        use self::Reason::*;

        match $reason {
            NoError => concat!($prefix, "not a result of an error"),
            ProtocolError => concat!($prefix, "unspecific protocol error detected"),
            InternalError => concat!($prefix, "unexpected internal error encountered"),
            FlowControlError => concat!($prefix, "flow-control protocol violated"),
            SettingsTimeout => concat!($prefix, "settings ACK not received in timely manner"),
            StreamClosed => concat!($prefix, "received frame when stream half-closed"),
            FrameSizeError => concat!($prefix, "frame sent with invalid size"),
            RefusedStream => concat!($prefix, "refused stream before processing any application logic"),
            Cancel => concat!($prefix, "stream no longer needed"),
            CompressionError => concat!($prefix, "unable to maintain the header compression context"),
            ConnectError => concat!($prefix, "connection established in response to a CONNECT request was reset or abnormally closed"),
            EnhanceYourCalm => concat!($prefix, "detected excessive load generating behavior"),
            InadequateSecurity => concat!($prefix, "security properties do not meet minimum requirements"),
            Http11Required => concat!($prefix, "endpoint requires HTTP/1.1"),
            Other(_) => concat!($prefix, "other reason (ain't no tellin')"),
        }
    });
}

macro_rules! user_desc {
    ($reason:expr) => (user_desc!($reason, ""));
    ($reason:expr, $prefix:expr) => ({
        use self::User::*;

        match $reason {
            InvalidStreamId => concat!($prefix, "invalid stream ID"),
            InactiveStreamId => concat!($prefix, "inactive stream ID"),
            UnexpectedFrameType => concat!($prefix, "unexpected frame type"),
            FlowControlViolation => concat!($prefix, "flow control violation"),
            StreamReset => concat!($prefix, "frame sent on reset stream"),
            Corrupt => concat!($prefix, "connection state corrupt"),
            MaxConcurrencyExceeded => concat!($prefix, "stream would exceed remote max concurrency"),
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

impl From<User> for ConnectionError {
    fn from(src: User) -> ConnectionError {
        ConnectionError::User(src)
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
            User(e) => write!(fmt, "user error: {}", e),
        }
    }
}

impl error::Error for ConnectionError {
    fn description(&self) -> &str {
        use self::ConnectionError::*;

        match *self {
            Io(ref e) => error::Error::description(e),
            Proto(reason) => reason_desc!(reason, "protocol error: "),
            User(user) => user_desc!(user, "user error: "),
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

// ===== impl User =====

impl User {
    pub fn description(&self) -> &str {
        user_desc!(*self)
    }
}

impl fmt::Display for User {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "{}", self.description())
    }
}
