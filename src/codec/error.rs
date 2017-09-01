use frame::{Reason, StreamId};

use std::io;

/// Errors that are received
#[derive(Debug)]
pub enum RecvError {
    Connection(Reason),
    Stream {
        id: StreamId,
        reason: Reason,
    },
    Io(io::Error),
}

/// Errors caused by sending a message
pub enum SendError {
    /// User error
    User(UserError),

    /// I/O error
    Io(io::Error),
}

/// Errors caused by users of the library
pub enum UserError {
    /// The stream ID is no longer accepting frames.
    InactiveStreamId,

    /// The stream is not currently expecting a frame of this type.
    UnexpectedFrameType,

    /// The payload size is too big
    PayloadTooBig,
}

// ===== impl RecvError =====

impl From<io::Error> for RecvError {
    fn from(src: io::Error) -> Self {
        RecvError::Io(src)
    }
}

// ===== impl SendError =====

impl From<io::Error> for SendError {
    fn from(src: io::Error) -> Self {
        SendError::Io(src)
    }
}

impl From<UserError> for SendError {
    fn from(src: UserError) -> Self {
        SendError::User(src)
    }
}
