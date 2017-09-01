use codec::UserError;
use std::{error, fmt, io};

pub use frame::Reason;

/// The error type for HTTP/2 operations
#[derive(Debug)]
pub struct Error {
    kind: Kind,
}

#[derive(Debug)]
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

// ===== impl Error =====

impl From<io::Error> for Error {
    fn from(src: io::Error) -> Error {
        Error { kind: Kind::Io(src) }
    }
}

/*
impl From<Reason> for Error {
    fn from(src: Reason) -> Error {
        Error::Proto(src)
    }
}
*/

impl From<UserError> for Error {
    fn from(src: UserError) -> Error {
        Error { kind: Kind::User(src) }
    }
}

/*
impl From<Error> for io::Error {
    fn from(src: Error) -> io::Error {
        io::Error::new(io::ErrorKind::Other, src)
    }
}

impl From<http::Error> for Error {
    fn from(_: http::Error) -> Self {
        // TODO: Should this always be a protocol error?
        Reason::ProtocolError.into()
    }
}

impl From<frame::Error> for Error {
    fn from(src: frame::Error) -> Error {
        match src {
            // TODO: implement
            _ => Error::Proto(Reason::ProtocolError),
        }
    }
}
*/

impl fmt::Display for Error {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        use self::Kind::*;

        match self.kind {
            Proto(reason) => write!(fmt, "protocol error: {}", reason),
            User(e) => write!(fmt, "user error: {}", e),
            Io(ref e) => fmt::Display::fmt(e, fmt),
        }
    }
}

impl error::Error for Error {
    fn description(&self) -> &str {
        use self::Kind::*;

        match self.kind {
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
