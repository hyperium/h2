use {frame, http};
use std::{error, fmt, io};

pub use frame::Reason;

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

    /// The payload size is too big
    PayloadTooBig,

    /// The connection state is corrupt and the connection should be dropped.
    Corrupt,

    /// The stream state has been reset.
    StreamReset(Reason),

    /// The application attempted to initiate too many streams to remote.
    Rejected,

    // TODO: reserve additional variants
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
            StreamReset(_) => concat!($prefix, "frame sent on reset stream"),
            Corrupt => concat!($prefix, "connection state corrupt"),
            Rejected => concat!($prefix, "stream would exceed remote max concurrency"),
            PayloadTooBig => concat!($prefix, "payload too big"),
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

impl From<http::Error> for ConnectionError {
    fn from(_: http::Error) -> Self {
        // TODO: Should this always be a protocol error?
        Reason::ProtocolError.into()
    }
}

impl From<frame::Error> for ConnectionError {
    fn from(src: frame::Error) -> ConnectionError {
        match src {
            // TODO: implement
            _ => ConnectionError::Proto(Reason::ProtocolError),
        }
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
            Proto(ref reason) => reason.description(),
            User(user) => user_desc!(user, "user error: "),
        }
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
