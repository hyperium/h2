//! Server implementation of the HTTP/2.0 protocol.
//!
//! # Getting started
//!
//! Running an HTTP/2.0 requires the caller to manage accepting the connections
//! as well as getting the connections to a state that is ready to begin the
//! HTTP/2.0 handshake. See [here](../index.html#handshake) for more details.
//!
//! The following example runs a basic HTTP/2.0 server with [prior knowledge],
//! i.e. both the client and the server assume that the TCP socket will use the
//! HTTP/2.0 protocol without any prior negotiation.
//!
//! Once a connection is obtained and primed (ALPN negotiation, HTTP/1.1
//! upgrade, etc...), the connection handle is passed to [`Server::handshake`],
//! which will begin the [HTTP/2.0 handshake]. This returns a future that will
//! complete once the handshake is complete and HTTP/2.0 streams may be
//! received.
//!
//! [`Server::handshake`] will use a default configuration. There are a number
//! of configuration values that can be set by using a [`Builder`] instead.
//!
//! # Accepting inbound streams
//!
//! The [`Server`] instance is used to accept inbound HTTP/2.0 streams as well
//! as to manage the connection state. Either [`Server::poll`] or
//! [`Server::poll_close`] must be called or no data will be sent to or received
//! from the connection.
//!
//! [prior knowledge]: (http://httpwg.org/specs/rfc7540.html#known-http)
//! [`Server::handshake`]: struct.Server.html#method.handshake
//! [HTTP/2.0 handshake]: http://httpwg.org/specs/rfc7540.html#ConnectionHeader
//! [`Builder`]: struct.Builder.html
//! [`Server`]: struct.Server.html
//! [`Server::poll`]: struct.Server.html#method.poll
//! [`Server::poll_close`]: struct.Server.html#method.poll_close
//!
//! # Example
//!
//! A basic HTTP/2.0 server example that runs over TCP and assumes prior
//! knowledge.
//!
//! ```rust
//! extern crate futures;
//! extern crate h2;
//! extern crate http;
//! extern crate tokio_core;
//!
//! use futures::{Future, Stream};
//! # use futures::future::ok;
//! use h2::server::Server;
//! use http::{Response, StatusCode};
//! use tokio_core::reactor;
//! use tokio_core::net::TcpListener;
//!
//! pub fn main () {
//!     let mut core = reactor::Core::new().unwrap();
//!     let handle = core.handle();
//!
//!     let addr = "127.0.0.1:5928".parse().unwrap();
//!     let listener = TcpListener::bind(&addr, &handle).unwrap();
//!
//!     core.run({
//!         // Accept all incoming TCP connections.
//!         listener.incoming().for_each(move |(socket, _)| {
//!             // Spawn a new task to process each connection.
//!             handle.spawn({
//!                 // Start the HTTP/2.0 connection handshake
//!                 Server::handshake(socket)
//!                     .and_then(|h2| {
//!                         // Accept all inbound HTTP/2.0 streams sent over the
//!                         // connection.
//!                         h2.for_each(|(request, mut respond)| {
//!                             println!("Received request: {:?}", request);
//!
//!                             // Build a response with no body
//!                             let response = Response::builder()
//!                                 .status(StatusCode::OK)
//!                                 .body(())
//!                                 .unwrap();
//!
//!                             // Send the response back to the client
//!                             respond.send_response(response, true)
//!                                 .unwrap();
//!
//!                             Ok(())
//!                         })
//!                     })
//!                     .map_err(|e| panic!("unexpected error = {:?}", e))
//!             });
//!
//!             Ok(())
//!         })
//!         # .select(ok(()))
//!     }).ok().expect("failed to run HTTP/2.0 server");
//! }
//! ```

use {SendStream, RecvStream, ReleaseCapacity};
use codec::{Codec, RecvError};
use frame::{self, Reason, Settings, StreamId};
use proto::{self, Connection, Prioritized};

use bytes::{Buf, Bytes, IntoBuf};
use futures::{self, Async, Future, Poll};
use http::{Request, Response};
use tokio_io::{AsyncRead, AsyncWrite};
use std::{convert, fmt, mem};

/// In progress H2 connection binding
#[must_use = "futures do nothing unless polled"]
pub struct Handshake<T, B: IntoBuf = Bytes> {
    /// SETTINGS frame that will be sent once the connection is established.
    settings: Settings,
    /// The current state of the handshake.
    state: Handshaking<T, B>
}

/// Accepts inbound HTTP/2.0 streams on a connection.
///
/// A `Server` is backed by an I/O resource (usually a TCP socket) and
/// implements the HTTP/2.0 server logic for that connection. It is responsible
/// for receiving inbound streams initiated by the client as well as driving the
/// internal state forward.
///
/// `Server` values are created by calling [`handshake`]. Once a `Server` value
/// is obtained, the caller must call [`poll`] or [`poll_close`] in order to
/// drive the internal connection state forward.
///
/// See [module level] documentation for more details
///
/// [module level]: index.html
/// [`handshake`]: struct.Server.html#method.handshake
/// [`poll`]: struct.Server.html#method.poll
/// [`poll_close`]: struct.Server.html#method.poll_close
///
/// # Examples
///
/// ```
/// ```
#[must_use = "streams do nothing unless polled"]
pub struct Server<T, B: IntoBuf> {
    connection: Connection<T, Peer, B>,
}

/// Server factory, which can be used in order to configure the properties of
/// the HTTP/2.0 server before it is created.
///
/// Methods can be changed on it in order to configure it.
///
/// The server is constructed by calling [`handshake`] and passing the I/O
/// handle that will back the HTTP/2.0 server.
///
/// New instances of `Builder` are obtained via [`Server::builder`].
///
/// See function level documentation for details on the various server
/// configuration settings.
///
/// [`Server::builder`]: struct.Server.html#method.builder
/// [`handshake`]: struct.Builder.html#method.handshake
///
/// # Examples
///
/// ```
/// # extern crate h2;
/// # extern crate tokio_io;
/// # use tokio_io::*;
/// # use h2::server::*;
/// #
/// # fn doc<T: AsyncRead + AsyncWrite>(my_io: T)
/// # -> Handshake<T>
/// # {
/// // `server_fut` is a future representing the completion of the HTTP/2.0
/// // handshake.
/// let server_fut = Server::builder()
///     .initial_window_size(1_000_000)
///     .max_concurrent_streams(1000)
///     .handshake(my_io);
/// # server_fut
/// # }
/// #
/// # pub fn main() {}
/// ```
#[derive(Clone, Debug, Default)]
pub struct Builder {
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
    fn handshake2(io: T, settings: Settings) -> Handshake<T, B> {
        // Create the codec.
        let mut codec = Codec::new(io);

        if let Some(max) = settings.max_frame_size() {
            codec.set_max_recv_frame_size(max as usize);
        }

        // Send initial settings frame.
        codec
            .buffer(settings.clone().into())
            .expect("invalid SETTINGS frame");

        // Create the handshake future.
        let state = Handshaking::from(codec);

        Handshake { settings, state }
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
    /// Indicates the initial window size (in octets) for stream-level
    /// flow control for received data.
    ///
    /// The initial window of a stream is used as part of flow control. For more
    /// details, see [`ReleaseCapacity`].
    ///
    /// The default value is 65,535.
    ///
    /// [`ReleaseCapacity`]: ../struct.ReleaseCapacity.html
    ///
    /// # Examples
    ///
    /// ```
    /// # extern crate h2;
    /// # extern crate tokio_io;
    /// # use tokio_io::*;
    /// # use h2::server::*;
    /// #
    /// # fn doc<T: AsyncRead + AsyncWrite>(my_io: T)
    /// # -> Handshake<T>
    /// # {
    /// // `server_fut` is a future representing the completion of the HTTP/2.0
    /// // handshake.
    /// let server_fut = Server::builder()
    ///     .initial_window_size(1_000_000)
    ///     .handshake(my_io);
    /// # server_fut
    /// # }
    /// #
    /// # pub fn main() {}
    /// ```
    pub fn initial_window_size(&mut self, size: u32) -> &mut Self {
        self.settings.set_initial_window_size(Some(size));
        self
    }

    /// Indicates the size (in octets) of the largest HTTP/2.0 frame payload that the
    /// configured server is able to accept.
    ///
    /// The sender may send data frames that are **smaller** than this value,
    /// but any data larger than `max` will be broken up into multiple `DATA`
    /// frames.
    ///
    /// The value **must** be between 16,384 and 16,777,215. The default value is 16,384.
    ///
    /// # Examples
    ///
    /// ```
    /// # extern crate h2;
    /// # extern crate tokio_io;
    /// # use tokio_io::*;
    /// # use h2::server::*;
    /// #
    /// # fn doc<T: AsyncRead + AsyncWrite>(my_io: T)
    /// # -> Handshake<T>
    /// # {
    /// // `server_fut` is a future representing the completion of the HTTP/2.0
    /// // handshake.
    /// let server_fut = Server::builder()
    ///     .max_frame_size(1_000_000)
    ///     .handshake(my_io);
    /// # server_fut
    /// # }
    /// #
    /// # pub fn main() {}
    /// ```
    ///
    /// # Panics
    ///
    /// This function panics if `max` is not within the legal range specified
    /// above.
    pub fn max_frame_size(&mut self, max: u32) -> &mut Self {
        self.settings.set_max_frame_size(Some(max));
        self
    }

    /// Set the maximum number of concurrent streams.
    ///
    /// The maximum concurrent streams setting only controls the maximum number
    /// of streams that can be initiated by the remote peer. In otherwords, when
    /// this setting is set to 100, this does not limit the number of concurrent
    /// streams that can be created by the caller.
    ///
    /// It is recommended that this value be no smaller than 100, so as to not
    /// unnecessarily limit parallelism. However, any value is legal, including
    /// 0. If `max` is set to 0, then the remote will not be permitted to
    /// initiate streams.
    ///
    /// Note that streams in the reserved state, i.e., push promises that have
    /// been reserved but the stream has not started, do not count against this
    /// setting.
    ///
    /// Also note that if the remote *does* exceed the value set here, it is not
    /// a protocol level error. Instead, the `h2` library will immediately reset
    /// the stream.
    ///
    /// See [Section 5.1.2] in the HTTP/2.0 spec for more details.
    ///
    /// [Section 5.1.2]: https://http2.github.io/http2-spec/#rfc.section.5.1.2
    ///
    /// # Examples
    ///
    /// ```
    /// # extern crate h2;
    /// # extern crate tokio_io;
    /// # use tokio_io::*;
    /// # use h2::server::*;
    /// #
    /// # fn doc<T: AsyncRead + AsyncWrite>(my_io: T)
    /// # -> Handshake<T>
    /// # {
    /// // `server_fut` is a future representing the completion of the HTTP/2.0
    /// // handshake.
    /// let server_fut = Server::builder()
    ///     .max_concurrent_streams(1000)
    ///     .handshake(my_io);
    /// # server_fut
    /// # }
    /// #
    /// # pub fn main() {}
    /// ```
    pub fn max_concurrent_streams(&mut self, max: u32) -> &mut Self {
        self.settings.set_max_concurrent_streams(Some(max));
        self
    }

    /// Create a new configured HTTP/2.0 server backed by `io`.
    ///
    /// It is expected that `io` already be in an appropriate state to commence
    /// the [HTTP/2.0 handshake]. See [Handshake] for more details.
    ///
    /// Returns a future which resolves to the [`Server`] value once the
    /// HTTP/2.0 handshake has been completed.
    ///
    /// This function also allows the caller to configure the send payload data
    /// type. See [Outbound data type] for more details.
    ///
    /// [HTTP/2.0 handshake]: http://httpwg.org/specs/rfc7540.html#ConnectionHeader
    /// [Handshake]: ../index.html#handshake
    /// [`Server`]: struct.Server.html
    /// [Outbound data type]: ../index.html#outbound-data-type.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```
    /// # extern crate h2;
    /// # extern crate tokio_io;
    /// # use tokio_io::*;
    /// # use h2::server::*;
    /// #
    /// # fn doc<T: AsyncRead + AsyncWrite>(my_io: T)
    /// # -> Handshake<T>
    /// # {
    /// // `server_fut` is a future representing the completion of the HTTP/2.0
    /// // handshake.
    /// let server_fut = Server::builder()
    ///     .handshake(my_io);
    /// # server_fut
    /// # }
    /// #
    /// # pub fn main() {}
    /// ```
    ///
    /// Customizing the outbound data type. In this case, the outbound data type
    /// will be `&'static [u8]`.
    ///
    /// ```
    /// # extern crate h2;
    /// # extern crate tokio_io;
    /// # use tokio_io::*;
    /// # use h2::server::*;
    /// #
    /// # fn doc<T: AsyncRead + AsyncWrite>(my_io: T)
    /// # -> Handshake<T, &'static [u8]>
    /// # {
    /// // `server_fut` is a future representing the completion of the HTTP/2.0
    /// // handshake.
    /// let server_fut: Handshake<_, &'static [u8]> = Server::builder()
    ///     .handshake(my_io);
    /// # server_fut
    /// # }
    /// #
    /// # pub fn main() {}
    /// ```
    pub fn handshake<T, B>(&self, io: T) -> Handshake<T, B>
    where
        T: AsyncRead + AsyncWrite,
        B: IntoBuf,
        B::Buf: 'static,
    {
        Server::handshake2(io, self.settings.clone())
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
            let connection =
                Connection::new(codec, &self.settings, 2.into());
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
