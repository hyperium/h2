use {frame, proto, Frame, Peer, ConnectionError, StreamId};

use http;

use futures::{Future, Poll};

use tokio_io::{AsyncRead, AsyncWrite};

/// In progress H2 connection binding
pub struct Handshake<T> {
    // TODO: unbox
    inner: Box<Future<Item = Connection<T>, Error = ConnectionError>>,
}

/// Marker type indicating a client peer
pub struct Client;

pub type Connection<T> = super::Connection<T, Client>;

/// Bind an H2 client connection.
///
/// Returns a future which resolves to the connection value once the H2
/// handshake has been completed.
pub fn bind<T>(io: T) -> Handshake<T>
    where T: AsyncRead + AsyncWrite + 'static,
{
    use tokio_io::io;

    debug!("binding client connection");

    let handshake = io::write_all(io, b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
        .map(|(io, _)| {
            debug!("client connection bound");
            proto::new_connection(io)
        })
        .map_err(ConnectionError::from);

    Handshake { inner: Box::new(handshake) }
}

impl Peer for Client {
    type Send = http::request::Head;
    type Poll = http::response::Head;

    fn check_initiating_id(id: StreamId) -> Result<(), ConnectionError> {
        if id % 2 == 0 {
            // Client stream identifiers must be odd
            unimplemented!();
        }

        // TODO: Ensure the `id` doesn't overflow u31

        Ok(())
    }

    fn convert_send_message(
        id: StreamId,
        message: Self::Send,
        body: bool) -> proto::SendMessage
    {
        use http::request::Head;

        // Extract the components of the HTTP request
        let Head { method, uri, headers, .. } = message;

        // Build the set pseudo header set. All requests will include `method`
        // and `path`.
        let mut pseudo = frame::Pseudo::request(method, uri.path().into());

        // If the URI includes a scheme component, add it to the pseudo headers
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

        // TODO: Factor in trailers
        if !body {
            frame.set_end_stream();
        } else {
            unimplemented!();
        }

        // Return the `SendMessage`
        proto::SendMessage::new(frame)
    }

    fn convert_poll_message(message: proto::PollMessage) -> Frame<Self::Poll> {
        unimplemented!();
    }
}

impl<T> Future for Handshake<T> {
    type Item = Connection<T>;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.inner.poll()
    }
}
