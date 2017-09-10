use frame::{StreamId, Headers, Pseudo, Settings};
use frame::Reason::*;
use codec::{Codec, RecvError};
use proto::{self, Connection, WindowSize};

use http::{Request, Response, HeaderMap};
use futures::{Future, Poll, Sink, Async, AsyncSink, AndThen, MapErr};
use tokio_io::{AsyncRead, AsyncWrite};
use tokio_io::io::WriteAll;
use bytes::{Bytes, IntoBuf};

use std::fmt;
use std::io::Error as IoError;

/// In progress H2 connection binding
pub struct Handshake<T: AsyncRead + AsyncWrite, B: IntoBuf = Bytes> {
    inner:
        AndThen<
            MapErr<WriteAll<T, &'static [u8]>, fn(IoError) -> ::Error>,
            Result<Client<T, B>, ::Error>,
            fn((T, &'static [u8])) -> Result<Client<T, B>, ::Error>
        >

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
    where T: AsyncRead + AsyncWrite,
{
    pub fn handshake(io: T) -> Handshake<T, Bytes> {
        Client::handshake2(io)
    }
}

impl<T, B> Client<T, B>
    where T: AsyncRead + AsyncWrite,
          B: IntoBuf
{
    /// Bind an H2 client connection.
    ///
    /// Returns a future which resolves to the connection value once the H2
    /// handshake has been completed.
    ///
    /// It's important to note that this does not **flush** the outbound
    /// settings to the wire.
    pub fn handshake2(io: T) -> Handshake<T, B> {
        use tokio_io::io;

        debug!("binding client connection");

        let bind: fn((T, &'static [u8]))
                    -> Result<Client<T, B>, ::Error> =
            |(io, _)| {
                debug!("client connection bound");

                // Create the codec
                let mut codec = Codec::new(io);

                // Create the initial SETTINGS frame
                let settings = Settings::default();

                // Send initial settings frame
                match codec.start_send(settings.into()) {
                    Ok(AsyncSink::Ready) => {
                        let connection = Connection::new(codec);
                        Ok(Client { connection })
                    }
                    Ok(_) => unreachable!(),
                    Err(e) => Err(::Error::from(e)),
                }
            };

        let msg: &'static [u8] =  b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
        let handshake = io::write_all(io, msg)
            .map_err(::Error::from as
                fn(IoError) -> ::Error
            )
            .and_then(bind);

        Handshake { inner: handshake }
    }

    /// Returns `Ready` when the connection can initialize a new HTTP 2.0
    /// stream.
    pub fn poll_ready(&mut self) -> Poll<(), ::Error> {
        Ok(self.connection.poll_send_request_ready())
    }

    /// Send a request on a new HTTP 2.0 stream
    pub fn request(&mut self, request: Request<()>, end_of_stream: bool)
        -> Result<Stream<B>, ::Error>
    {
        self.connection.send_request(request, end_of_stream)
            .map_err(Into::into)
            .map(|stream| Stream {
                inner: stream,
            })
    }
}

impl<T, B> Future for Client<T, B>
    // TODO: Get rid of 'static
    where T: AsyncRead + AsyncWrite,
          B: IntoBuf,
{
    type Item = ();
    type Error = ::Error;

    fn poll(&mut self) -> Poll<(), ::Error> {
        self.connection.poll()
            .map_err(Into::into)
    }
}

impl<T, B> fmt::Debug for Client<T, B>
    where T: AsyncRead + AsyncWrite,
          T: fmt::Debug,
          B: fmt::Debug + IntoBuf,
          B::Buf: fmt::Debug,
{
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("Client")
            .field("connection", &self.connection)
            .finish()
    }
}

#[cfg(feature = "unstable")]
impl<T, B> Client<T, B>
    where T: AsyncRead + AsyncWrite,
          B: IntoBuf
{
    /// Returns the number of active streams.
    ///
    /// An active stream is a stream that has not yet transitioned to a closed
    /// state.
    pub fn num_active_streams(&self) -> usize {
        self.connection.num_active_streams()
    }

    /// Returns the number of streams that are held in memory.
    ///
    /// A wired stream is a stream that is either active or is closed but must
    /// stay in memory for some reason. For example, there are still outstanding
    /// userspace handles pointing to the slot.
    pub fn num_wired_streams(&self) -> usize {
        self.connection.num_wired_streams()
    }
}

// ===== impl Handshake =====

impl<T, B: IntoBuf> Future for Handshake<T, B>
where T: AsyncRead + AsyncWrite {
    type Item = Client<T, B>;
    type Error = ::Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.inner.poll()
    }
}

impl<T, B> fmt::Debug for Handshake<T, B>
    where T: AsyncRead + AsyncWrite,
          T: fmt::Debug,
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
    pub fn poll_response(&mut self) -> Poll<Response<Body<B>>, ::Error> {
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
    pub fn poll_capacity(&mut self) -> Poll<Option<usize>, ::Error> {
        let res = try_ready!(self.inner.poll_capacity());
        Ok(Async::Ready(res.map(|v| v as usize)))
    }

    /// Send data
    pub fn send_data(&mut self, data: B, end_of_stream: bool)
        -> Result<(), ::Error>
    {
        self.inner.send_data(data.into_buf(), end_of_stream)
            .map_err(Into::into)
    }

    /// Send trailers
    pub fn send_trailers(&mut self, trailers: HeaderMap)
        -> Result<(), ::Error>
    {
        self.inner.send_trailers(trailers)
            .map_err(Into::into)
    }
}

impl<B: IntoBuf> Future for Stream<B> {
    type Item = Response<Body<B>>;
    type Error = ::Error;

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

    pub fn release_capacity(&mut self, sz: usize) -> Result<(), ::Error> {
        self.inner.release_capacity(sz as proto::WindowSize)
            .map_err(Into::into)
    }

    /// Poll trailers
    ///
    /// This function **must** not be called until `Body::poll` returns `None`.
    pub fn poll_trailers(&mut self) -> Poll<Option<HeaderMap>, ::Error> {
        self.inner.poll_trailers()
            .map_err(Into::into)
    }
}

impl<B: IntoBuf> ::futures::Stream for Body<B> {
    type Item = Bytes;
    type Error = ::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        self.inner.poll_data()
            .map_err(Into::into)
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
        end_of_stream: bool) -> Headers
    {
        use http::request::Parts;

        let (Parts { method, uri, headers, .. }, _) = request.into_parts();

        // Build the set pseudo header set. All requests will include `method`
        // and `path`.
        let pseudo = Pseudo::request(method, uri);

        // Create the HEADERS frame
        let mut frame = Headers::new(id, pseudo, headers);

        if end_of_stream {
            frame.set_end_stream()
        }

        frame
    }

    fn convert_poll_message(headers: Headers) -> Result<Self::Poll, RecvError> {
        let mut b = Response::builder();

        let stream_id = headers.stream_id();
        let (pseudo, fields) = headers.into_parts();

        if let Some(status) = pseudo.status {
            b.status(status);
        }

        let mut response = match b.body(()) {
            Ok(response) => response,
            Err(_) => {
                // TODO: Should there be more specialized handling for different
                // kinds of errors
                return Err(RecvError::Stream {
                    id: stream_id,
                    reason: ProtocolError,
                });
            }
        };

        *response.headers_mut() = fields;

        Ok(response)
    }
}
