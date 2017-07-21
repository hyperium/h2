use {frame, proto, Peer, ConnectionError, StreamId};

use http;
use futures::{Future, Poll};
use tokio_io::{AsyncRead, AsyncWrite};
use bytes::{Bytes, IntoBuf};

use std::fmt;

/// In progress H2 connection binding
pub struct Handshake<T, B: IntoBuf = Bytes> {
    // TODO: unbox
    inner: Box<Future<Item = Connection<T, B>, Error = ConnectionError>>,
}

/// Marker type indicating a client peer
#[derive(Debug)]
pub struct Client;

pub type Connection<T, B = Bytes> = super::Connection<T, Client, B>;

pub fn handshake<T>(io: T) -> Handshake<T, Bytes>
    where T: AsyncRead + AsyncWrite + 'static,
{
    handshake2(io)
}

/// Bind an H2 client connection.
///
/// Returns a future which resolves to the connection value once the H2
/// handshake has been completed.
pub fn handshake2<T, B: IntoBuf>(io: T) -> Handshake<T, B>
    where T: AsyncRead + AsyncWrite + 'static,
{
    use tokio_io::io;

    debug!("binding client connection");

    let handshake = io::write_all(io, b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
        .map(|(io, _)| {
            debug!("client connection bound");

            // Use default local settings for now
            proto::from_io(io, Default::default())
        })
        .map_err(ConnectionError::from);

    Handshake { inner: Box::new(handshake) }
}

impl Peer for Client {
    type Send = http::request::Head;
    type Poll = http::response::Head;

    fn is_valid_local_stream_id(id: StreamId) -> bool {
        id.is_client_initiated()
    }

    fn is_valid_remote_stream_id(id: StreamId) -> bool {
        id.is_server_initiated()
    }

    fn local_can_open() -> bool {
        true
    }

    fn convert_send_message(
        id: StreamId,
        headers: Self::Send,
        end_of_stream: bool) -> frame::Headers
    {
        use http::request::Head;

        // Extract the components of the HTTP request
        let Head { method, uri, headers, .. } = headers;

        // TODO: Ensure that the version is set to H2

        // Build the set pseudo header set. All requests will include `method`
        // and `path`.
        let mut pseudo = frame::Pseudo::request(method, uri.path().into());

        // If the URI includes a scheme component, add it to the pseudo headers
        //
        // TODO: Scheme must be set...
        if let Some(scheme) = uri.scheme() {
            pseudo.set_scheme(scheme.into());
        }

        // If the URI includes an authority component, add it to the pseudo
        // headers
        if let Some(authority) = uri.authority() {
            pseudo.set_authority(authority.into());
        }

        // Create the HEADERS frame
        let mut frame = frame::Headers::new(id, pseudo, headers);

        if end_of_stream {
            frame.set_end_stream()
        }

        frame
    }

    fn convert_poll_message(headers: frame::Headers) -> Self::Poll {
        headers.into_response()
    }
}

impl<T, B: IntoBuf> Future for Handshake<T, B> {
    type Item = Connection<T, B>;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.inner.poll()
    }
}

impl<T, B> fmt::Debug for Handshake<T, B>
    where T: fmt::Debug,
          B: fmt::Debug + IntoBuf,
{
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "client::Handshake")
    }
}
