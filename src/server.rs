use {frame, ConnectionError, StreamId};
use {Body, Chunk};
use proto::{self, Connection};
use error::Reason::*;

use http::{self, Request, Response};
use futures::{self, Future, Sink, Poll, Async, AsyncSink, IntoFuture};
use tokio_io::{AsyncRead, AsyncWrite};
use bytes::{Bytes, IntoBuf};

use std::fmt;

/// In progress H2 connection binding
pub struct Handshake<T, B: IntoBuf = Bytes> {
    // TODO: unbox
    inner: Box<Future<Item = Server<T, B>, Error = ConnectionError>>,
}

/// Marker type indicating a client peer
pub struct Server<T, B: IntoBuf> {
    connection: Connection<T, Peer, B>,
}

#[derive(Debug)]
pub struct Stream<B: IntoBuf> {
    inner: proto::StreamRef<B::Buf>,
}

/// Flush a Sink
struct Flush<T> {
    inner: Option<T>,
}

/// Read the client connection preface
struct ReadPreface<T> {
    inner: Option<T>,
    pos: usize,
}

#[derive(Debug)]
pub(crate) struct Peer;

const PREFACE: [u8; 24] = *b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

// ===== impl Server =====

impl<T> Server<T, Bytes>
    where T: AsyncRead + AsyncWrite + 'static,
{
    pub fn handshake(io: T) -> Handshake<T, Bytes> {
        Server::handshake2(io)
    }
}

impl<T, B> Server<T, B>
    where T: AsyncRead + AsyncWrite + 'static,
          B: IntoBuf + 'static,
{
    /// Bind an H2 server connection.
    ///
    /// Returns a future which resolves to the connection value once the H2
    /// handshake has been completed.
    pub fn handshake2(io: T) -> Handshake<T, B> {
        let mut framed_write = proto::framed_write(io);
        let settings = frame::Settings::default();

       // Send initial settings frame
        match framed_write.start_send(settings.into()) {
            Ok(AsyncSink::Ready) => {}
            Ok(_) => unreachable!(),
            Err(e) => {
                return Handshake {
                    inner: Box::new(Err(ConnectionError::from(e)).into_future()),
                }
            }
        }

        // Flush pending settings frame and then wait for the client preface
        let handshake = Flush::new(framed_write)
            .and_then(ReadPreface::new)
            .map(move |framed_write| {
                let connection = proto::from_framed_write(framed_write);
                Server { connection }
            })
            ;

        Handshake { inner: Box::new(handshake) }
    }

    pub fn poll_close(&mut self) -> Poll<(), ConnectionError> {
        self.connection.poll()
    }
}

impl<T, B> futures::Stream for Server<T, B>
    where T: AsyncRead + AsyncWrite + 'static,
          B: IntoBuf + 'static,
{
    type Item = (Request<Body<B>>, Stream<B>);
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Option<Self::Item>, ConnectionError> {
        // Always try to advance the internal state. Getting NotReady also is
        // needed to allow this function to return NotReady.
        match self.poll_close()? {
            Async::Ready(_) => {
                // If the socket is closed, don't return anything
                // TODO: drop any pending streams
                return Ok(None.into())
            }
            _ => {}
        }

        if let Some(inner) = self.connection.next_incoming() {
            let (head, _) = inner.take_request()?.into_parts();
            let body = Body { inner: inner.clone() };

            let request = Request::from_parts(head, body);
            let incoming = Stream { inner };

            return Ok(Some((request, incoming)).into());
        }

        Ok(Async::NotReady)
    }
}

impl<T, B> fmt::Debug for Server<T, B>
    where T: fmt::Debug,
          B: fmt::Debug + IntoBuf,
          B::Buf: fmt::Debug,
{
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("Server")
            .field("connection", &self.connection)
            .finish()
    }
}

// ===== impl Stream =====

impl<B: IntoBuf> Stream<B> {
    pub fn send_response(&mut self, response: Response<()>, end_of_stream: bool)
        -> Result<(), ConnectionError>
    {
        self.inner.send_response(response, end_of_stream)
    }

    /// Send data
    pub fn send_data(&mut self, data: B, end_of_stream: bool)
        -> Result<(), ConnectionError>
    {
        self.inner.send_data::<Peer>(data.into_buf(), end_of_stream)
    }

    /// Send trailers
    pub fn send_trailers(&mut self, trailers: ())
        -> Result<(), ConnectionError>
    {
        unimplemented!();
    }
}

// ===== impl Flush =====

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

// ===== impl Handshake =====

impl<T, B: IntoBuf> Future for Handshake<T, B> {
    type Item = Server<T, B>;
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
        write!(fmt, "server::Handshake")
    }
}

impl proto::Peer for Peer {
    type Send = Response<()>;
    type Poll = Request<()>;

    fn is_server() -> bool {
        true
    }

    fn convert_send_message(
        id: StreamId,
        response: Self::Send,
        end_of_stream: bool) -> frame::Headers
    {
        use http::response::Parts;

        // Extract the components of the HTTP request
        let (Parts { status, headers, .. }, _) = response.into_parts();

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

    fn convert_poll_message(headers: frame::Headers)
        -> Result<Self::Poll, ConnectionError>
    {
        headers.into_request()
            // TODO: Is this always a protocol error?
            .map_err(|_| ProtocolError.into())
    }
}
