use {SendStream, RecvStream, ReleaseCapacity};
use codec::{Codec, RecvError};
use frame::{Headers, Pseudo, Reason, Settings, StreamId};
use proto;

use bytes::{Bytes, IntoBuf};
use futures::{Async, Future, MapErr, Poll};
use http::{Request, Response};
use tokio_io::{AsyncRead, AsyncWrite};
use tokio_io::io::WriteAll;

use std::fmt;
use std::io;
use std::marker::PhantomData;

/// In progress H2 connection binding
pub struct Handshake<T: AsyncRead + AsyncWrite, B: IntoBuf = Bytes> {
    builder: Builder,
    inner: MapErr<WriteAll<T, &'static [u8]>, fn(io::Error) -> ::Error>,
    _marker: PhantomData<B>,
}

/// Marker type indicating a client peer
pub struct Client<B: IntoBuf> {
    inner: proto::Streams<B::Buf, Peer>,
    pending: Option<proto::StreamKey>,
}

pub struct Connection<T, B: IntoBuf> {
    inner: proto::Connection<T, Peer, B>,
}

#[derive(Debug)]
pub struct ResponseFuture {
    inner: proto::OpaqueStreamRef,
}

/// Build a Client.
#[derive(Clone, Debug)]
pub struct Builder {
    settings: Settings,
    stream_id: StreamId,
}

#[derive(Debug)]
pub(crate) struct Peer;

// ===== impl Client =====

impl Client<Bytes> {
    /// Bind an H2 client connection.
    ///
    /// Returns a future which resolves to the connection value once the H2
    /// handshake has been completed.
    ///
    /// It's important to note that this does not **flush** the outbound
    /// settings to the wire.
    pub fn handshake<T>(io: T) -> Handshake<T, Bytes>
    where
        T: AsyncRead + AsyncWrite,
    {
        Builder::default().handshake(io)
    }
}

impl Client<Bytes> {
    /// Creates a Client Builder to customize a Client before binding.
    pub fn builder() -> Builder {
        Builder::default()
    }
}

impl<B> Client<B>
where
    B: IntoBuf,
    B::Buf: 'static,
{
    fn handshake2<T>(io: T, builder: Builder) -> Handshake<T, B>
    where
        T: AsyncRead + AsyncWrite,
    {
        use tokio_io::io;

        debug!("binding client connection");

        let msg: &'static [u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
        let handshake = io::write_all(io, msg).map_err(::Error::from as _);

        Handshake {
            builder,
            inner: handshake,
            _marker: PhantomData,
        }
    }

    /// Returns `Ready` when the connection can initialize a new HTTP 2.0
    /// stream.
    pub fn poll_ready(&mut self) -> Poll<(), ::Error> {
        try_ready!(self.inner.poll_pending_open(self.pending.as_ref()));
        self.pending = None;
        Ok(().into())
    }

    /// Send a request on a new HTTP 2.0 stream
    pub fn send_request(
        &mut self,
        request: Request<()>,
        end_of_stream: bool,
    ) -> Result<(ResponseFuture, SendStream<B>), ::Error> {
        self.inner
            .send_request(request, end_of_stream, self.pending.as_ref())
            .map_err(Into::into)
            .map(|stream| {
                if stream.is_pending_open() {
                    self.pending = Some(stream.key());
                }

                let response = ResponseFuture {
                    inner: stream.clone_to_opaque(),
                };

                let stream = SendStream::new(stream);

                (response, stream)
            })
    }
}

impl<B> fmt::Debug for Client<B>
where
    B: IntoBuf,
{
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("Client").finish()
    }
}

impl<B> Clone for Client<B>
where
    B: IntoBuf,
{
    fn clone(&self) -> Self {
        Client {
            inner: self.inner.clone(),
            pending: None,
        }
    }
}

#[cfg(feature = "unstable")]
impl<B> Client<B>
where
    B: IntoBuf,
{
    /// Returns the number of active streams.
    ///
    /// An active stream is a stream that has not yet transitioned to a closed
    /// state.
    pub fn num_active_streams(&self) -> usize {
        self.inner.num_active_streams()
    }

    /// Returns the number of streams that are held in memory.
    ///
    /// A wired stream is a stream that is either active or is closed but must
    /// stay in memory for some reason. For example, there are still outstanding
    /// userspace handles pointing to the slot.
    pub fn num_wired_streams(&self) -> usize {
        self.inner.num_wired_streams()
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
    /// Clients can only limit the maximum number of streams that that the
    /// server can initiate. See [Section 5.1.2] in the HTTP/2 spec for more
    /// details.
    ///
    /// [Section 5.1.2]: https://http2.github.io/http2-spec/#rfc.section.5.1.2
    pub fn max_concurrent_streams(&mut self, max: u32) -> &mut Self {
        self.settings.set_max_concurrent_streams(Some(max));
        self
    }

    /// Enable or disable the server to send push promises.
    pub fn enable_push(&mut self, enabled: bool) -> &mut Self {
        self.settings.set_enable_push(enabled);
        self
    }

    /// Set the first stream ID to something other than 1.
    #[cfg(feature = "unstable")]
    pub fn initial_stream_id(&mut self, stream_id: u32) -> &mut Self {
        self.stream_id = stream_id.into();
        assert!(
            self.stream_id.is_client_initiated(),
            "stream id must be odd"
        );
        self
    }

    /// Bind an H2 client connection.
    ///
    /// Returns a future which resolves to the connection value once the H2
    /// handshake has been completed.
    ///
    /// It's important to note that this does not **flush** the outbound
    /// settings to the wire.
    pub fn handshake<T, B>(&self, io: T) -> Handshake<T, B>
    where
        T: AsyncRead + AsyncWrite,
        B: IntoBuf,
        B::Buf: 'static,
    {
        Client::handshake2(io, self.clone())
    }
}

impl Default for Builder {
    fn default() -> Builder {
        Builder {
            settings: Default::default(),
            stream_id: 1.into(),
        }
    }
}

// ===== impl Connection =====


impl<T, B> Connection<T, B>
where
    T: AsyncRead + AsyncWrite,
    B: IntoBuf,
{
    /// Sets the target window size for the whole connection.
    ///
    /// Default in HTTP2 is 65_535.
    pub fn set_target_window_size(&mut self, size: u32) {
        assert!(size <= proto::MAX_WINDOW_SIZE);
        self.inner.set_target_window_size(size);
    }
}

impl<T, B> Future for Connection<T, B>
where
    T: AsyncRead + AsyncWrite,
    B: IntoBuf,
{
    type Item = ();
    type Error = ::Error;

    fn poll(&mut self) -> Poll<(), ::Error> {
        self.inner.poll().map_err(Into::into)
    }
}

impl<T, B> fmt::Debug for Connection<T, B>
where
    T: AsyncRead + AsyncWrite,
    T: fmt::Debug,
    B: fmt::Debug + IntoBuf,
    B::Buf: fmt::Debug,
{
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&self.inner, fmt)
    }
}

// ===== impl Handshake =====

impl<T, B> Future for Handshake<T, B>
where
    T: AsyncRead + AsyncWrite,
    B: IntoBuf,
    B::Buf: 'static,
{
    type Item = (Client<B>, Connection<T, B>);
    type Error = ::Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let (io, _) = try_ready!(self.inner.poll());

        debug!("client connection bound");

        // Create the codec
        let mut codec = Codec::new(io);

        if let Some(max) = self.builder.settings.max_frame_size() {
            codec.set_max_recv_frame_size(max as usize);
        }

        // Send initial settings frame
        codec
            .buffer(self.builder.settings.clone().into())
            .expect("invalid SETTINGS frame");

        let connection =
            proto::Connection::new(codec, &self.builder.settings, self.builder.stream_id);
        let client = Client {
            inner: connection.streams().clone(),
            pending: None,
        };
        let conn = Connection {
            inner: connection,
        };
        Ok(Async::Ready((client, conn)))
    }
}

impl<T, B> fmt::Debug for Handshake<T, B>
where
    T: AsyncRead + AsyncWrite,
    T: fmt::Debug,
    B: fmt::Debug + IntoBuf,
    B::Buf: fmt::Debug + IntoBuf,
{
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "client::Handshake")
    }
}

// ===== impl ResponseFuture =====

impl Future for ResponseFuture {
    type Item = Response<RecvStream>;
    type Error = ::Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let (parts, _) = try_ready!(self.inner.poll_response()).into_parts();
        let body = RecvStream::new(ReleaseCapacity::new(self.inner.clone()));

        Ok(Response::from_parts(parts, body).into())
    }
}

// ===== impl Peer =====

impl proto::Peer for Peer {
    type Send = Request<()>;
    type Poll = Response<()>;


    fn dyn() -> proto::DynPeer {
        proto::DynPeer::Client
    }

    fn is_server() -> bool {
        false
    }

    fn convert_send_message(id: StreamId, request: Self::Send, end_of_stream: bool) -> Headers {
        use http::request::Parts;

        let (
            Parts {
                method,
                uri,
                headers,
                ..
            },
            _,
        ) = request.into_parts();

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
                    reason: Reason::PROTOCOL_ERROR,
                });
            },
        };

        *response.headers_mut() = fields;

        Ok(response)
    }
}
