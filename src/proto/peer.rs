use codec::RecvError;
use error::Reason;
use frame::{Headers, StreamId};

use http::{Request, Response};

use std::fmt;

/// Either a Client or a Server
pub trait Peer {
    /// Message type sent into the transport
    type Send;

    /// Message type polled from the transport
    type Poll: fmt::Debug;

    fn dyn() -> Dyn;

    fn is_server() -> bool;

    fn convert_send_message(id: StreamId, headers: Self::Send, end_of_stream: bool) -> Headers;

    fn convert_poll_message(headers: Headers) -> Result<Self::Poll, RecvError>;

    fn is_local_init(id: StreamId) -> bool {
        assert!(!id.is_zero());
        Self::is_server() == id.is_server_initiated()
    }
}

/// A dynamic representation of `Peer`.
///
/// This is used internally to avoid incurring a generic on all internal types.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Dyn {
    Client,
    Server,
}

#[derive(Debug)]
pub enum PollMessage {
    Client(Response<()>),
    Server(Request<()>),
}

// ===== impl Dyn =====

impl Dyn {
    pub fn is_server(&self) -> bool {
        *self == Dyn::Server
    }

    pub fn is_local_init(&self, id: StreamId) -> bool {
        assert!(!id.is_zero());
        self.is_server() == id.is_server_initiated()
    }

    pub fn convert_poll_message(&self, headers: Headers) -> Result<PollMessage, RecvError> {
        if self.is_server() {
            ::server::Peer::convert_poll_message(headers)
                .map(PollMessage::Server)
        } else {
            ::client::Peer::convert_poll_message(headers)
                .map(PollMessage::Client)
        }
    }

    /// Returns true if the remote peer can initiate a stream with the given ID.
    pub fn ensure_can_open(&self, id: StreamId) -> Result<(), RecvError> {
        if !self.is_server() {
            // Remote is a server and cannot open streams. PushPromise is
            // registered by reserving, so does not go through this path.
            return Err(RecvError::Connection(Reason::PROTOCOL_ERROR));
        }

        // Ensure that the ID is a valid server initiated ID
        if !id.is_client_initiated() {
            return Err(RecvError::Connection(Reason::PROTOCOL_ERROR));
        }

        Ok(())
    }
}
