use codec::{Codec, RecvError};
use frame::{self, Reason, Settings, StreamId};
use proto::{self, Connection, WindowSize};

use bytes::{Buf, Bytes, IntoBuf};
use futures::{self, Async, Future, Poll};
use http::{HeaderMap, Request, Response};
use tokio_io::{AsyncRead, AsyncWrite};

use std::fmt;

/// In progress H2 connection binding
pub struct Handshake<T, B: IntoBuf = Bytes> {
    // TODO: unbox
    inner: Box<Future<Item = Server<T, B>, Error = ::Error>>,
}

/// Marker type indicating a client peer
pub struct Server<T, B: IntoBuf> {
    connection: Connection<T, Peer, B>,
}

/// Build a Server
#[derive(Clone, Debug, Default)]
pub struct Builder {
    settings: Settings,
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
pub struct Send<T> {
    src: T,
    dst: Option<Stream<Bytes>>,
    // Pending data
    buf: Option<Bytes>,
    // True when this is the end of the stream
    eos: bool,
}

/// Flush a Sink
struct Flush<T, B> {
    codec: Option<Codec<T, B>>,
}

/// Read the client connection preface
struct ReadPreface<T, B> {
    codec: Option<Codec<T, B>>,
    pos: usize,
}

#[derive(Debug)]
pub(crate) struct Peer;

const PREFACE: [u8; 24] = *b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

// ===== impl Server =====

impl<T> Server<T, Bytes>
where
    T: AsyncRead + AsyncWrite + 'static,
{
    /// Bind an H2 server connection.
    ///
    /// Returns a future which resolves to the connection value once the H2
    /// handshake has been completed.
    pub fn handshake(io: T) -> Handshake<T, Bytes> {
        Server::builder().handshake(io)
    }
}

impl Server<(), Bytes> {
    /// Create a Server Builder
    pub fn builder() -> Builder {
        Builder::default()
    }
}

impl<T, B> Server<T, B>
where
    T: AsyncRead + AsyncWrite + 'static,
    B: IntoBuf + 'static,
{
    fn handshake2(io: T, settings: Settings) -> Handshake<T, B> {
        // Create the codec
        let mut codec = Codec::new(io);

        if let Some(max) = settings.max_frame_size() {
            codec.set_max_recv_frame_size(max as usize);
        }

        // Send initial settings frame
        codec
            .buffer(settings.clone().into())
            .expect("invalid SETTINGS frame");

        // Flush pending settings frame and then wait for the client preface
        let handshake = Flush::new(codec)
            .and_then(ReadPreface::new)
            .map(move |codec| {
                let connection = Connection::new(codec, &settings, 2.into());
                Server {
                    connection,
                }
            });

        Handshake {
            inner: Box::new(handshake),
        }
    }

    /// Returns `Ready` when the underlying connection has closed.
    pub fn poll_close(&mut self) -> Poll<(), ::Error> {
        self.connection.poll().map_err(Into::into)
    }
}

impl<T, B> futures::Stream for Server<T, B>
where
    T: AsyncRead + AsyncWrite + 'static,
    B: IntoBuf + 'static,
{
    type Item = (Request<Body<B>>, Stream<B>);
    type Error = ::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, ::Error> {
        // Always try to advance the internal state. Getting NotReady also is
        // needed to allow this function to return NotReady.
        match self.poll_close()? {
            Async::Ready(_) => {
                // If the socket is closed, don't return anything
                // TODO: drop any pending streams
                return Ok(None.into());
            },
            _ => {},
        }

        if let Some(inner) = self.connection.next_incoming() {
            trace!("received incoming");
            let (head, _) = inner.take_request().into_parts();
            let body = Body {
                inner: inner.clone(),
            };

            let request = Request::from_parts(head, body);
            let incoming = Stream {
                inner,
            };

            return Ok(Some((request, incoming)).into());
        }

        Ok(Async::NotReady)
    }
}

impl<T, B> fmt::Debug for Server<T, B>
where
    T: fmt::Debug,
    B: fmt::Debug + IntoBuf,
    B::Buf: fmt::Debug,
{
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("Server")
            .field("connection", &self.connection)
            .finish()
    }
}

// ===== impl Builder =====

impl Builder {
    /// Set the initial window size of the remote peer.
    pub fn initial_window_size(&mut self, size: u32) -> &mut Self {
        self.settings.set_initial_window_size(Some(size));
        self
    }

    /// Set the max frame size of received frames.
    pub fn max_frame_size(&mut self, max: u32) -> &mut Self {
        self.settings.set_max_frame_size(Some(max));
        self
    }

    /// Bind an H2 server connection.
    ///
    /// Returns a future which resolves to the connection value once the H2
    /// handshake has been completed.
    pub fn handshake<T, B>(&self, io: T) -> Handshake<T, B>
    where
        T: AsyncRead + AsyncWrite + 'static,
        B: IntoBuf + 'static,
    {
        Server::handshake2(io, self.settings.clone())
    }
}

// ===== impl Stream =====

impl<B: IntoBuf> Stream<B> {
    /// Send a response
    pub fn send_response(
        &mut self,
        response: Response<()>,
        end_of_stream: bool,
    ) -> Result<(), ::Error> {
        self.inner
            .send_response(response, end_of_stream)
            .map_err(Into::into)
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
    pub fn poll_capacity(&mut self) -> Poll<Option<usize>, ::Error> {
        let res = try_ready!(self.inner.poll_capacity());
        Ok(Async::Ready(res.map(|v| v as usize)))
    }

    /// Send a single data frame
    pub fn send_data(&mut self, data: B, end_of_stream: bool) -> Result<(), ::Error> {
        self.inner
            .send_data(data.into_buf(), end_of_stream)
            .map_err(Into::into)
    }

    /// Send trailers
    pub fn send_trailers(&mut self, trailers: HeaderMap) -> Result<(), ::Error> {
        self.inner.send_trailers(trailers).map_err(Into::into)
    }

    pub fn send_reset(mut self, reason: Reason) {
        self.inner.send_reset(reason)
    }
}

impl Stream<Bytes> {
    /// Send the body
    pub fn send<T>(self, src: T, end_of_stream: bool) -> Send<T>
    where
        T: futures::Stream<Item = Bytes, Error = ::Error>,
    {
        Send {
            src: src,
            dst: Some(self),
            buf: None,
            eos: end_of_stream,
        }
    }
}

// ===== impl Body =====

impl<B: IntoBuf> Body<B> {
    pub fn is_empty(&self) -> bool {
        // If the recv side is closed and the receive queue is empty, the body is empty.
        self.inner.body_is_empty()
    }

    pub fn release_capacity(&mut self, sz: usize) -> Result<(), ::Error> {
        self.inner
            .release_capacity(sz as proto::WindowSize)
            .map_err(Into::into)
    }

    /// Poll trailers
    ///
    /// This function **must** not be called until `Body::poll` returns `None`.
    pub fn poll_trailers(&mut self) -> Poll<Option<HeaderMap>, ::Error> {
        self.inner.poll_trailers().map_err(Into::into)
    }
}

impl<B: IntoBuf> futures::Stream for Body<B> {
    type Item = Bytes;
    type Error = ::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        self.inner.poll_data().map_err(Into::into)
    }
}

// ===== impl Send =====

impl<T> Future for Send<T>
where
    T: futures::Stream<Item = Bytes, Error = ::Error>,
{
    type Item = Stream<Bytes>;
    type Error = ::Error;

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
                },
                None => {
                    // TODO: It would be nice to not have to send an extra
                    // frame...
                    if self.eos {
                        self.dst.as_mut().unwrap().send_data(Bytes::new(), true)?;
                    }

                    return Ok(Async::Ready(self.dst.take().unwrap()));
                },
            }
        }
    }
}

// ===== impl Flush =====

impl<T, B: Buf> Flush<T, B> {
    fn new(codec: Codec<T, B>) -> Self {
        Flush {
            codec: Some(codec),
        }
    }
}

impl<T, B> Future for Flush<T, B>
where
    T: AsyncWrite,
    B: Buf,
{
    type Item = Codec<T, B>;
    type Error = ::Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        // Flush the codec
        try_ready!(self.codec.as_mut().unwrap().flush());

        // Return the codec
        Ok(Async::Ready(self.codec.take().unwrap()))
    }
}

impl<T, B: Buf> ReadPreface<T, B> {
    fn new(codec: Codec<T, B>) -> Self {
        ReadPreface {
            codec: Some(codec),
            pos: 0,
        }
    }

    fn inner_mut(&mut self) -> &mut T {
        self.codec.as_mut().unwrap().get_mut()
    }
}

impl<T, B> Future for ReadPreface<T, B>
where
    T: AsyncRead,
    B: Buf,
{
    type Item = Codec<T, B>;
    type Error = ::Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let mut buf = [0; 24];
        let mut rem = PREFACE.len() - self.pos;

        while rem > 0 {
            let n = try_nb!(self.inner_mut().read(&mut buf[..rem]));

            if PREFACE[self.pos..self.pos + n] != buf[..n] {
                // TODO: Should this just write the GO_AWAY frame directly?
                return Err(Reason::ProtocolError.into());
            }

            self.pos += n;
            rem -= n; // TODO test
        }

        Ok(Async::Ready(self.codec.take().unwrap()))
    }
}

// ===== impl Handshake =====

impl<T, B: IntoBuf> Future for Handshake<T, B> {
    type Item = Server<T, B>;
    type Error = ::Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.inner.poll()
    }
}

impl<T, B> fmt::Debug for Handshake<T, B>
where
    T: fmt::Debug,
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
        end_of_stream: bool,
    ) -> frame::Headers {
        use http::response::Parts;

        // Extract the components of the HTTP request
        let (
            Parts {
                status,
                headers,
                ..
            },
            _,
        ) = response.into_parts();

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

    fn convert_poll_message(headers: frame::Headers) -> Result<Self::Poll, RecvError> {
        use http::{uri, Version};

        let mut b = Request::builder();

        let stream_id = headers.stream_id();
        let (pseudo, fields) = headers.into_parts();

        macro_rules! malformed {
            () => {
                return Err(RecvError::Stream {
                    id: stream_id,
                    reason: Reason::ProtocolError,
                });
            }
        };

        b.version(Version::HTTP_2);

        if let Some(method) = pseudo.method {
            b.method(method);
        } else {
            malformed!();
        }

        // Specifying :status for a request is a protocol error
        if pseudo.status.is_some() {
            return Err(RecvError::Connection(Reason::ProtocolError));
        }

        // Convert the URI
        let mut parts = uri::Parts::default();

        if let Some(scheme) = pseudo.scheme {
            // TODO: Don't unwrap
            parts.scheme = Some(uri::Scheme::from_shared(scheme.into_inner()).unwrap());
        } else {
            malformed!();
        }

        if let Some(authority) = pseudo.authority {
            // TODO: Don't unwrap
            parts.authority = Some(uri::Authority::from_shared(authority.into_inner()).unwrap());
        }

        if let Some(path) = pseudo.path {
            // This cannot be empty
            if path.is_empty() {
                malformed!();
            }

            // TODO: Don't unwrap
            parts.path_and_query = Some(uri::PathAndQuery::from_shared(path.into_inner()).unwrap());
        }

        b.uri(parts);

        let mut request = match b.body(()) {
            Ok(request) => request,
            Err(_) => {
                // TODO: Should there be more specialized handling for different
                // kinds of errors
                return Err(RecvError::Stream {
                    id: stream_id,
                    reason: Reason::ProtocolError,
                });
            },
        };

        *request.headers_mut() = fields;

        Ok(request)
    }
}
