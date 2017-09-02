use frame::Reason;
use codec::RecvError;

use std::io;

/// Either an H2 reason  or an I/O error
#[derive(Debug)]
pub enum Error {
    Proto(Reason),
    Io(io::Error),
}

impl Error {
    pub fn into_connection_recv_error(self) -> RecvError {
        use self::Error::*;

        match self {
            Proto(reason) => RecvError::Connection(reason),
            Io(e) => RecvError::Io(e),
        }
    }
}

impl From<Reason> for Error {
    fn from(src: Reason) -> Self {
        Error::Proto(src)
    }
}

impl From<io::Error> for Error {
    fn from(src: io::Error) -> Self {
        Error::Io(src)
    }
}
