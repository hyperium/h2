//! HTTP2 server side.

use {SendStream, RecvStream, ReleaseCapacity};
use codec::{Codec, RecvError};
use frame::{self, Reason, Settings, StreamId};
use proto::{self, Config, Connection, Prioritized};

use bytes::{Buf, Bytes, IntoBuf};
use futures::{self, Async, Future, Poll};
use http::{Request, Response};
use tokio_io::{AsyncRead, AsyncWrite};
use std::{convert, fmt, mem};
use std::time::Duration;

/// In progress H2 connection binding
#[must_use = "futures do nothing unless polled"]
pub struct Handshake<T, B: IntoBuf = Bytes> {
    /// The config to pass to Connection::new after handshake succeeds.
    builder: Builder,
    /// The current state of the handshake.
    state: Handshaking<T, B>
}

/// Marker type indicating a client peer
#[must_use = "streams do nothing unless polled"]
pub struct Server<T, B: IntoBuf> {
    connection: Connection<T, Peer, B>,
}

/// Build a Server
#[derive(Clone, Debug)]
pub struct Builder {
    /// Time to keep locally reset streams around before reaping.
    reset_stream_duration: Duration,

    /// Maximum number of locally reset streams to keep at a time.
    reset_stream_max: usize,

    /// Initial `Settings` frame to send as part of the handshake.
    settings: Settings,
}

/// Respond to a request
///
///
/// Instances of `Respond` are used to send a response or reserve push promises.
#[derive(Debug)]
pub struct Respond<B: IntoBuf> {
    inner: proto::StreamRef<B::Buf>,
}

/// Stages of an in-progress handshake.
enum Handshaking<T, B: IntoBuf> {
    /// State 1. Server is flushing pending SETTINGS frame.
    Flushing(Flush<T, Prioritized<B::Buf>>),
    /// State 2. Server is waiting for the client preface.
    ReadingPreface(ReadPreface<T, Prioritized<B::Buf>>),
    /// Dummy state for `mem::replace`.
    Empty,
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
    T: AsyncRead + AsyncWrite,
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
    T: AsyncRead + AsyncWrite,
    B: IntoBuf,
    B::Buf: 'static,
{
    fn handshake2(io: T, builder: Builder) -> Handshake<T, B> {
        // Create the codec.
        let mut codec = Codec::new(io);

        if let Some(max) = builder.settings.max_frame_size() {
            codec.set_max_recv_frame_size(max as usize);
        }

        // Send initial settings frame.
        codec
            .buffer(builder.settings.clone().into())
            .expect("invalid SETTINGS frame");

        // Create the handshake future.
        let state = Handshaking::from(codec);

        Handshake { builder, state }
    }

    /// Sets the target window size for the whole connection.
    ///
    /// Default in HTTP2 is 65_535.
    pub fn set_target_window_size(&mut self, size: u32) {
        assert!(size <= proto::MAX_WINDOW_SIZE);
        self.connection.set_target_window_size(size);
    }

    /// Returns `Ready` when the underlying connection has closed.
    pub fn poll_close(&mut self) -> Poll<(), ::Error> {
        self.connection.poll().map_err(Into::into)
}
}

impl<T, B> futures::Stream for Server<T, B>
where
    T: AsyncRead + AsyncWrite,
    B: IntoBuf,
    B::Buf: 'static,
{
    type Item = (Request<RecvStream>, Respond<B>);
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
            let body = RecvStream::new(ReleaseCapacity::new(inner.clone_to_opaque()));

            let request = Request::from_parts(head, body);
            let respond = Respond { inner };

            return Ok(Some((request, respond)).into());
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

    /// Set the maximum number of concurrent streams.
    ///
    /// Servers can only limit the maximum number of streams that that the
    /// client can initiate. See [Section 5.1.2] in the HTTP/2 spec for more
    /// details.
    ///
    /// [Section 5.1.2]: https://http2.github.io/http2-spec/#rfc.section.5.1.2
    pub fn max_concurrent_streams(&mut self, max: u32) -> &mut Self {
        self.settings.set_max_concurrent_streams(Some(max));
        self
    }

    /// Set the maximum number of concurrent locally reset streams.
    ///
    /// Locally reset streams are to "ignore frames from the peer for some
    /// time". While waiting for that time, locally reset streams "waste"
    /// space in order to be able to ignore those frames. This setting
    /// can limit how many extra streams are left waiting for "some time".
    pub fn max_concurrent_reset_streams(&mut self, max: usize) -> &mut Self {
        self.reset_stream_max = max;
        self
    }

    /// Set the maximum number of concurrent locally reset streams.
    ///
    /// Locally reset streams are to "ignore frames from the peer for some
    /// time", but that time is unspecified. Set that time with this setting.
    pub fn reset_stream_duration(&mut self, dur: Duration) -> &mut Self {
        self.reset_stream_duration = dur;
        self
    }

    /// Bind an H2 server connection.
    ///
    /// Returns a future which resolves to the connection value once the H2
    /// handshake has been completed.
    pub fn handshake<T, B>(&self, io: T) -> Handshake<T, B>
    where
        T: AsyncRead + AsyncWrite,
        B: IntoBuf,
        B::Buf: 'static,
    {
        Server::handshake2(io, self.clone())
    }
}

impl Default for Builder {
    fn default() -> Builder {
        Builder {
            reset_stream_duration: Duration::from_secs(proto::DEFAULT_RESET_STREAM_SECS),
            reset_stream_max: proto::DEFAULT_RESET_STREAM_MAX,
            settings: Settings::default(),
        }
    }
}

// ===== impl Respond =====

impl<B: IntoBuf> Respond<B> {
    /// Send a response
    pub fn send_response(
        &mut self,
        response: Response<()>,
        end_of_stream: bool,
    ) -> Result<SendStream<B>, ::Error> {
        self.inner
            .send_response(response, end_of_stream)
            .map(|_| SendStream::new(self.inner.clone()))
            .map_err(Into::into)
    }

    /// Reset the stream
    pub fn send_reset(&mut self, reason: Reason) {
        self.inner.send_reset(reason)
    }

    // TODO: Support reserving push promises.
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
                return Err(Reason::PROTOCOL_ERROR.into());
            }

            self.pos += n;
            rem -= n; // TODO test
        }

        Ok(Async::Ready(self.codec.take().unwrap()))
    }
}

// ===== impl Handshake =====

impl<T, B: IntoBuf> Future for Handshake<T, B>
    where T: AsyncRead + AsyncWrite,
          B: IntoBuf,
{
    type Item = Server<T, B>;
    type Error = ::Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        trace!("Handshake::poll(); state={:?};", self.state);
        use server::Handshaking::*;

        self.state = if let Flushing(ref mut flush) = self.state {
            // We're currently flushing a pending SETTINGS frame. Poll the
            // flush future, and, if it's completed, advance our state to wait
            // for the client preface.
            let codec = match flush.poll()? {
                Async::NotReady => {
                    trace!("Handshake::poll(); flush.poll()=NotReady");
                    return Ok(Async::NotReady);
                },
                Async::Ready(flushed) => {
                    trace!("Handshake::poll(); flush.poll()=Ready");
                    flushed
                }
            };
            Handshaking::from(ReadPreface::new(codec))
        } else {
            // Otherwise, we haven't actually advanced the state, but we have
            // to replace it with itself, because we have to return a value.
            // (note that the assignment to `self.state` has to be outside of
            // the `if let` block above in order to placate the borrow checker).
            mem::replace(&mut self.state, Handshaking::Empty)
        };
        let poll = if let ReadingPreface(ref mut read) = self.state {
            // We're now waiting for the client preface. Poll the `ReadPreface`
            // future. If it has completed, we will create a `Server` handle
            // for the connection.
            read.poll()
            // Actually creating the `Connection` has to occur outside of this
            // `if let` block, because we've borrowed `self` mutably in order
            // to poll the state and won't be able to borrow the SETTINGS frame
            // as well until we release the borrow for `poll()`.
        } else {
            unreachable!("Handshake::poll() state was not advanced completely!")
        };
        let server = poll?.map(|codec| {
            let connection = Connection::new(codec, Config {
                next_stream_id: 2.into(),
                reset_stream_duration: self.builder.reset_stream_duration,
                reset_stream_max: self.builder.reset_stream_max,
                settings: self.builder.settings.clone(),
            });
            trace!("Handshake::poll(); connection established!");
            Server { connection }
        });
        Ok(server)
    }
}

impl<T, B> fmt::Debug for Handshake<T, B>
    where T: AsyncRead + AsyncWrite + fmt::Debug,
          B: fmt::Debug + IntoBuf,
{
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "server::Handshake")
    }
}

impl Peer {
    pub fn convert_send_message(
        id: StreamId,
        response: Response<()>,
        end_of_stream: bool) -> frame::Headers
    {
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
}

impl proto::Peer for Peer {
    type Poll = Request<()>;

    fn is_server() -> bool {
        true
    }

    fn dyn() -> proto::DynPeer {
        proto::DynPeer::Server
    }

    fn convert_poll_message(headers: frame::Headers) -> Result<Self::Poll, RecvError> {
        use http::{uri, Version};

        let mut b = Request::builder();

        let stream_id = headers.stream_id();
        let (pseudo, fields) = headers.into_parts();

        macro_rules! malformed {
            ($($arg:tt)*) => {{
                debug!($($arg)*);
                return Err(RecvError::Stream {
                    id: stream_id,
                    reason: Reason::PROTOCOL_ERROR,
                });
            }}
        };

        b.version(Version::HTTP_2);

        if let Some(method) = pseudo.method {
            b.method(method);
        } else {
            malformed!("malformed headers: missing method");
        }

        // Specifying :status for a request is a protocol error
        if pseudo.status.is_some() {
            return Err(RecvError::Connection(Reason::PROTOCOL_ERROR));
        }

        // Convert the URI
        let mut parts = uri::Parts::default();

        if let Some(scheme) = pseudo.scheme {
            parts.scheme = Some(uri::Scheme::from_shared(scheme.into_inner())
                .or_else(|_| malformed!("malformed headers: malformed scheme"))?);
        } else {
            malformed!("malformed headers: missing scheme");
        }

        if let Some(authority) = pseudo.authority {
            parts.authority = Some(uri::Authority::from_shared(authority.into_inner())
                .or_else(|_| malformed!("malformed headers: malformed authority"))?);
        }

        if let Some(path) = pseudo.path {
            // This cannot be empty
            if path.is_empty() {
                malformed!("malformed headers: missing path");
            }

            parts.path_and_query = Some(uri::PathAndQuery::from_shared(path.into_inner())
                .or_else(|_| malformed!("malformed headers: malformed path"))?);
        }

        b.uri(parts);

        let mut request = match b.body(()) {
            Ok(request) => request,
            Err(_) => {
                // TODO: Should there be more specialized handling for different
                // kinds of errors
                return Err(RecvError::Stream {
                    id: stream_id,
                    reason: Reason::PROTOCOL_ERROR,
                });
            },
        };

        *request.headers_mut() = fields;

        Ok(request)
    }
}



// ===== impl Handshaking =====
impl<T, B> fmt::Debug for Handshaking<T, B>
where
    B: IntoBuf
{
    #[inline] fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        match *self {
            Handshaking::Flushing(_) =>
                write!(f, "Handshaking::Flushing(_)"),
            Handshaking::ReadingPreface(_) =>
                write!(f, "Handshaking::ReadingPreface(_)"),
            Handshaking::Empty =>
                write!(f, "Handshaking::Empty"),
        }

    }
}

impl<T, B> convert::From<Flush<T, Prioritized<B::Buf>>> for Handshaking<T, B>
where
    T: AsyncRead + AsyncWrite,
    B: IntoBuf,
{
    #[inline] fn from(flush: Flush<T, Prioritized<B::Buf>>) -> Self {
        Handshaking::Flushing(flush)
    }
}

impl<T, B> convert::From<ReadPreface<T, Prioritized<B::Buf>>> for
    Handshaking<T, B>
where
    T: AsyncRead + AsyncWrite,
    B: IntoBuf,
{
    #[inline] fn from(read: ReadPreface<T, Prioritized<B::Buf>>) -> Self {
        Handshaking::ReadingPreface(read)
    }
}

impl<T, B> convert::From<Codec<T, Prioritized<B::Buf>>> for Handshaking<T, B>
where
    T: AsyncRead + AsyncWrite,
    B: IntoBuf,
{
    #[inline] fn from(codec: Codec<T, Prioritized<B::Buf>>) -> Self {
        Handshaking::from(Flush::new(codec))
    }
}
