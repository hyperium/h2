use frame::{Headers, StreamId};
use codec::RecvError;

use std::fmt;

/// Either a Client or a Server
pub trait Peer {
    /// Message type sent into the transport
    type Send;

    /// Message type polled from the transport
    type Poll: fmt::Debug;

    fn is_server() -> bool;

    fn convert_send_message(
        id: StreamId,
        headers: Self::Send,
        end_of_stream: bool) -> Headers;

    fn convert_poll_message(headers: Headers) -> Result<Self::Poll, RecvError>;
}
