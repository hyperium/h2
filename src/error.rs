use {frame, http};
use codec::{SendError, RecvError, UserError};
use std::{error, fmt, io};

pub use frame::Reason;

/// The error type for HTTP/2 operations
#[derive(Debug)]
pub struct Error {
    kind: Kind,
}

enum Kind {
    /// An error caused by an action taken by the remote peer.
    ///
    /// This is either an error received by the peer or caused by an invalid
    /// action taken by the peer (i.e. a protocol error).
    Proto(Reason),

    /// An error resulting from an invalid action taken by the user of this
    /// library.
    User(UserError),

    /// An `io::Error` occurred while trying to read or write.
    Io(io::Error),
}

// ===== impl ConnectionError =====

impl From<io::Error> for ConnectionError {
    fn from(src: io::Error) -> ConnectionError {
        ConnectionError::Io(src)
    }
}

/*
impl From<Reason> for ConnectionError {
    fn from(src: Reason) -> ConnectionError {
        ConnectionError::Proto(src)
    }
}
*/

impl From<UserError> for ConnectionError {
    fn from(src: UserError) -> ConnectionError {
        ConnectionError::UserError(src)
    }
}

/*
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
*/

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
            User(user) => user.description(),
        }
    }
}

// ===== impl User =====

/*
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
*/
