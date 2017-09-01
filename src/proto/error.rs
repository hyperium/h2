use frame::Reason;

use std::io;

/// Either an H2 reason  or an I/O error
pub enum Error {
    Proto(Reason),
    Io(io::Error),
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
