use {frame, proto, Peer, ConnectionError, StreamId};

use http;
use futures::{Future, Poll, Sink, AsyncSink};
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
pub fn handshake2<T, B>(io: T) -> Handshake<T, B>
    where T: AsyncRead + AsyncWrite + 'static,
          B: IntoBuf + 'static,
{
    use tokio_io::io;

    debug!("binding client connection");

    let handshake = io::write_all(io, b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
        .map_err(ConnectionError::from)
        .and_then(|(io, _)| {
            debug!("client connection bound");

            let mut framed_write = proto::framed_write(io);
            let settings = frame::Settings::default();

            // Send initial settings frame
            match framed_write.start_send(settings.into()) {
                Ok(AsyncSink::Ready) => {
                    Ok(proto::from_framed_write(framed_write))
                }
                Ok(_) => unreachable!(),
                Err(e) => Err(ConnectionError::from(e)),
            }
        });

    Handshake { inner: Box::new(handshake) }
}

impl Peer for Client {
    type Send = http::request::Head;
    type Poll = http::response::Head;

    fn is_server() -> bool {
        false
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
        let pseudo = frame::Pseudo::request(method, uri);

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
