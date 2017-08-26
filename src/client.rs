use {frame, HeaderMap, ConnectionError};
use frame::StreamId;
use proto::{self, Connection, WindowSize};

use http::{Request, Response};
use futures::{Future, Poll, Sink, Async, AsyncSink};
use tokio_io::{AsyncRead, AsyncWrite};
use bytes::{Bytes, IntoBuf};

use std::fmt;

/// In progress H2 connection binding
pub struct Handshake<T, B: IntoBuf = Bytes> {
    // TODO: unbox
    inner: Box<Future<Item = Client<T, B>, Error = ConnectionError>>,
}

/// Marker type indicating a client peer
pub struct Client<T, B: IntoBuf> {
    connection: Connection<T, Peer, B>,
}

#[derive(Debug)]
pub struct Stream<B: IntoBuf> {
    inner: proto::StreamRef<B::Buf, Peer>,
}

#[derive(Debug)]
pub struct Body<B: IntoBuf> {
    inner: proto::StreamRef<B::Buf, Peer>,
}

#[derive(Debug)]
pub(crate) struct Peer;

impl<T> Client<T, Bytes>
    where T: AsyncRead + AsyncWrite + 'static,
{
    pub fn handshake(io: T) -> Handshake<T, Bytes> {
        Client::handshake2(io)
    }
}

impl<T, B> Client<T, B>
    // TODO: Get rid of 'static
    where T: AsyncRead + AsyncWrite + 'static,
          B: IntoBuf + 'static,
{
    /// Bind an H2 client connection.
    ///
    /// Returns a future which resolves to the connection value once the H2
    /// handshake has been completed.
    pub fn handshake2(io: T) -> Handshake<T, B> {
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
                        let connection = proto::from_framed_write(framed_write);
                        Ok(Client { connection })
                    }
                    Ok(_) => unreachable!(),
                    Err(e) => Err(ConnectionError::from(e)),
                }
            });

        Handshake { inner: Box::new(handshake) }
    }

    /// Returns `Ready` when the connection can initialize a new HTTP 2.0
    /// stream.
    pub fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        self.connection.poll_send_request_ready()
    }

    /// Send a request on a new HTTP 2.0 stream
    pub fn request(&mut self, request: Request<()>, end_of_stream: bool)
        -> Result<Stream<B>, ConnectionError>
    {
        self.connection.send_request(request, end_of_stream)
            .map(|stream| Stream {
                inner: stream,
            })
    }
}

impl<T, B> Future for Client<T, B>
    // TODO: Get rid of 'static
    where T: AsyncRead + AsyncWrite + 'static,
          B: IntoBuf + 'static,
{
    type Item = ();
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<(), ConnectionError> {
        self.connection.poll()
    }
}

impl<T, B> fmt::Debug for Client<T, B>
    where T: fmt::Debug,
          B: fmt::Debug + IntoBuf,
          B::Buf: fmt::Debug,
{
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("Client")
            .field("connection", &self.connection)
            .finish()
    }
}

// ===== impl Handshake =====

impl<T, B: IntoBuf> Future for Handshake<T, B> {
    type Item = Client<T, B>;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.inner.poll()
    }
}

impl<T, B> fmt::Debug for Handshake<T, B>
    where T: fmt::Debug,
          B: fmt::Debug + IntoBuf,
          B::Buf: fmt::Debug + IntoBuf,
{
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "client::Handshake")
    }
}

// ===== impl Stream =====

impl<B: IntoBuf> Stream<B> {
    /// Receive the HTTP/2.0 response, if it is ready.
    pub fn poll_response(&mut self) -> Poll<Response<Body<B>>, ConnectionError> {
        let (parts, _) = try_ready!(self.inner.poll_response()).into_parts();
        let body = Body { inner: self.inner.clone() };

        Ok(Response::from_parts(parts, body).into())
    }

    /// Request capacity to send data
    pub fn reserve_capacity(&mut self, capacity: usize) {
        // TODO: Check for overflow
        self.inner.reserve_capacity(capacity as WindowSize)
    }

    /// Returns the stream's current send capacity.
    pub fn capacity(&self) -> usize {
        self.inner.capacity() as usize
    }

    /// Request to be notified when the stream's capacity increases
    pub fn poll_capacity(&mut self) -> Poll<Option<usize>, ConnectionError> {
        let res = try_ready!(self.inner.poll_capacity());
        Ok(Async::Ready(res.map(|v| v as usize)))
    }

    /// Send data
    pub fn send_data(&mut self, data: B, end_of_stream: bool)
        -> Result<(), ConnectionError>
    {
        self.inner.send_data(data.into_buf(), end_of_stream)
    }

    /// Send trailers
    pub fn send_trailers(&mut self, trailers: HeaderMap)
        -> Result<(), ConnectionError>
    {
        self.inner.send_trailers(trailers)
    }
}

impl<B: IntoBuf> Future for Stream<B> {
    type Item = Response<Body<B>>;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.poll_response()
    }
}

// ===== impl Body =====

impl<B: IntoBuf> Body<B> {
    pub fn is_empty(&self) -> bool {
        // If the recv side is closed and the receive queue is empty, the body is empty.
        self.inner.body_is_empty()
    }

    pub fn release_capacity(&mut self, sz: usize) -> Result<(), ConnectionError> {
        self.inner.release_capacity(sz as proto::WindowSize)
    }

    /// Poll trailers
    ///
    /// This function **must** not be called until `Body::poll` returns `None`.
    pub fn poll_trailers(&mut self) -> Poll<Option<HeaderMap>, ConnectionError> {
        self.inner.poll_trailers()
    }
}

impl<B: IntoBuf> ::futures::Stream for Body<B> {
    type Item = Bytes;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        self.inner.poll_data()
    }
}

// ===== impl Peer =====

impl proto::Peer for Peer {
    type Send = Request<()>;
    type Poll = Response<()>;

    fn is_server() -> bool {
        false
    }

    fn convert_send_message(
        id: StreamId,
        request: Self::Send,
        end_of_stream: bool) -> frame::Headers
    {
        use http::request::Parts;

        let (Parts { method, uri, headers, .. }, _) = request.into_parts();

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

    fn convert_poll_message(headers: frame::Headers) -> Result<Self::Poll, ConnectionError> {
        headers.into_response()
    }
}
