use {Body, HeaderMap, ConnectionError};
use frame::{self, StreamId};
use proto::{self, Connection, WindowSize};
use error::Reason;
use error::Reason::*;

use http::{Request, Response};
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

#[derive(Debug)]
pub struct Send<T> {
    src: T,
    dst: Option<Stream<Bytes>>,
    // Pending data
    buf: Option<Bytes>,
    // True when this is the end of the stream
    eos: bool,
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

    /// Returns `Ready` when the underlying connection has closed.
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
            trace!("received incoming");
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
    /// Send a response
    pub fn send_response(&mut self, response: Response<()>, end_of_stream: bool)
        -> Result<(), ConnectionError>
    {
        self.inner.send_response(response, end_of_stream)
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

    /// Send a single data frame
    pub fn send_data(&mut self, data: B, end_of_stream: bool)
        -> Result<(), ConnectionError>
    {
        self.inner.send_data::<Peer>(data.into_buf(), end_of_stream)
    }

    /// Send trailers
    pub fn send_trailers(&mut self, trailers: HeaderMap)
        -> Result<(), ConnectionError>
    {
        self.inner.send_trailers::<Peer>(trailers)
    }

    pub fn send_reset(mut self, reason: Reason) -> Result<(), ConnectionError> {
        self.inner.send_reset::<Peer>(reason)
    }
}

impl Stream<Bytes> {
    /// Send the body
    pub fn send<T>(self, src: T, end_of_stream: bool,) -> Send<T>
        where T: futures::Stream<Item = Bytes, Error = ConnectionError>,
    {
        Send {
            src: src,
            dst: Some(self),
            buf: None,
            eos: end_of_stream,
        }
    }
}

// ===== impl Send =====

impl<T> Future for Send<T>
    where T: futures::Stream<Item = Bytes, Error = ConnectionError>,
{
    type Item = Stream<Bytes>;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            if self.buf.is_none() {
                // Get a chunk to send to the H2 stream
                self.buf = try_ready!(self.src.poll());
            }

            match self.buf.take() {
                Some(mut buf) => {
                    let dst = self.dst.as_mut().unwrap();

                    // Ask for the amount of capacity needed
                    dst.reserve_capacity(buf.len());

                    let cap = dst.capacity();

                    if cap == 0 {
                        self.buf = Some(buf);
                        // TODO: This seems kind of lame :(
                        try_ready!(dst.poll_capacity());
                        continue;
                    }

                    let chunk = buf.split_to(cap);

                    if !buf.is_empty() {
                        self.buf = Some(buf);
                    }

                    dst.send_data(chunk, false)?;
                }
                None => {
                    // TODO: It would be nice to not have to send an extra
                    // frame...
                    if self.eos {
                        self.dst.as_mut().unwrap().send_data(Bytes::new(), true)?;
                    }

                    return Ok(Async::Ready(self.dst.take().unwrap()));
                }
            }
        }
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
                // TODO: Should this just write the GO_AWAY frame directly?
                return Err(ProtocolError.into());
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
