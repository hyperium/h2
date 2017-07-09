use {frame, proto, Peer, ConnectionError, StreamId};

use http;
use futures::{Future, Sink, Poll, Async};
use tokio_io::{AsyncRead, AsyncWrite};

use std::fmt;

/// In progress H2 connection binding
pub struct Handshake<T> {
    // TODO: unbox
    inner: Box<Future<Item = Connection<T>, Error = ConnectionError>>,
}

/// Marker type indicating a client peer
#[derive(Debug)]
pub struct Server;

pub type Connection<T> = super::Connection<T, Server>;

/// Flush a Sink
struct Flush<T> {
    inner: Option<T>,
}

/// Read the client connection preface
struct ReadPreface<T> {
    inner: Option<T>,
    pos: usize,
}

const PREFACE: [u8; 24] = *b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// Bind an H2 server connection.
///
/// Returns a future which resolves to the connection value once the H2
/// handshake has been completed.
pub fn handshake<T>(io: T) -> Handshake<T>
    where T: AsyncRead + AsyncWrite + 'static,
{
    let transport = proto::server_handshaker(io, Default::default());

    // Flush pending settings frame and then wait for the client preface
    let handshake = Flush::new(transport)
        .and_then(ReadPreface::new)
        .map(proto::from_server_handshaker)
        ;

    Handshake { inner: Box::new(handshake) }
}

impl<T> Flush<T> {
    fn new(inner: T) -> Self {
        Flush { inner: Some(inner) }
    }
}

impl<T: Sink> Future for Flush<T> {
    type Item = T;
    type Error = T::SinkError;

    fn poll(&mut self) -> Poll<T, Self::Error> {
        try_ready!(self.inner.as_mut().unwrap().poll_complete());
        Ok(Async::Ready(self.inner.take().unwrap()))
    }
}

impl<T> ReadPreface<T> {
    fn new(inner: T) -> Self {
        ReadPreface {
            inner: Some(inner),
            pos: 0,
        }
    }
}

impl<T: AsyncRead> Future for ReadPreface<T> {
    type Item = T;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<T, Self::Error> {
        let mut buf = [0; 24];
        let mut rem = PREFACE.len() - self.pos;

        while rem > 0 {
            let n = try_nb!(self.inner.as_mut().unwrap().read(&mut buf[..rem]));

            if PREFACE[self.pos..self.pos+n] != buf[..n] {
                // TODO: Invalid connection preface
                unimplemented!();
            }

            self.pos += n;
            rem -= n; // TODO test
        }

        Ok(Async::Ready(self.inner.take().unwrap()))
    }
}

impl Peer for Server {
    type Send = http::response::Head;
    type Poll = http::request::Head;

    fn is_valid_local_stream_id(id: StreamId) -> bool {
        id.is_server_initiated()
    }

    fn is_valid_remote_stream_id(id: StreamId) -> bool {
        id.is_client_initiated()
    }

    fn convert_send_message(
        id: StreamId,
        headers: Self::Send,
        end_of_stream: bool) -> frame::Headers
    {
        use http::response::Head;

        // Extract the components of the HTTP request
        let Head { status, headers, .. } = headers;

        // TODO: Ensure that the version is set to H2

        // Build the set pseudo header set. All requests will include `method`
        // and `path`.
        let pseudo = frame::Pseudo::response(status);

        // Create the HEADERS frame
        let mut frame = frame::Headers::new(id, pseudo, headers);

        if end_of_stream {
            frame.set_end_stream()
        }

        frame
    }

    fn convert_poll_message(headers: frame::Headers) -> Self::Poll {
        headers.into_request()
    }
}

impl<T> Future for Handshake<T> {
    type Item = Connection<T>;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.inner.poll()
    }
}

impl<T: fmt::Debug> fmt::Debug for Handshake<T> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "server::Handshake")
    }
}
