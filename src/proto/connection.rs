use {frame, Frame, ConnectionError, Peer, StreamId};
use proto::{self, ReadySink, State};

use tokio_io::{AsyncRead, AsyncWrite};
use tokio_io::codec::length_delimited;

use http;

use futures::*;

use ordermap::OrderMap;
use fnv::FnvHasher;

use std::marker::PhantomData;
use std::hash::BuildHasherDefault;

/// An H2 connection
pub struct Connection<T, P> {
    inner: Inner<T>,
    streams: StreamMap<State>,
    peer: PhantomData<P>,
}

type Inner<T> =
    proto::Settings<
        proto::PingPong<
            proto::FramedWrite<
                proto::FramedRead<
                    length_delimited::FramedRead<T>>>>>;

type StreamMap<T> = OrderMap<StreamId, T, BuildHasherDefault<FnvHasher>>;

/// Returns a new `Connection` backed by the given `io`.
pub fn new<T, P>(io: T) -> Connection<T, P>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
{

    // Delimit the frames
    let framed_read = length_delimited::Builder::new()
        .big_endian()
        .length_field_length(3)
        .length_adjustment(9)
        .num_skip(0) // Don't skip the header
        .new_read(io);

    // Map to `Frame` types
    let framed_read = proto::FramedRead::new(framed_read);

    // Frame encoder
    let mut framed = proto::FramedWrite::new(framed_read);

    // Ok, so this is a **little** hacky, but it works for now.
    //
    // The ping/pong behavior SHOULD be given highest priority (6.7).
    // However, the connection handshake requires the settings frame to be
    // sent as the very first one. This needs special handling because
    // otherwise there is a race condition where the peer could send its
    // settings frame followed immediately by a Ping, in which case, we
    // don't want to accidentally send the pong before finishing the
    // connection hand shake.
    //
    // So, to ensure correct ordering, we write the settings frame here
    // before fully constructing the connection struct. Technically, `Async`
    // operations should not be performed in `new` because this might not
    // happen on a task, however we have full control of the I/O and we know
    // that the settings frame will get buffered and not actually perform an
    // I/O op.
    let initial_settings = frame::SettingSet::default();
    let frame = frame::Settings::new(initial_settings.clone());
    assert!(framed.start_send(frame.into()).unwrap().is_ready());

    // Add ping/pong handler
    let ping_pong = proto::PingPong::new(framed);

    // Add settings handler
    let connection = proto::Settings::new(ping_pong, initial_settings);

    Connection {
        inner: connection,
        streams: StreamMap::default(),
        peer: PhantomData,
    }
}

impl<T, P> Connection<T, P>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
{
    /// Completes when the connection has terminated
    pub fn poll_shutdown(&mut self) -> Poll<(), ConnectionError> {
        try_ready!(self.poll_complete());
        Ok(Async::NotReady)
    }
}

impl<T, P> Stream for Connection<T, P>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
{
    type Item = Frame<P::Poll>;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Option<Self::Item>, ConnectionError> {
        use frame::Frame::*;

        match try_ready!(self.inner.poll()) {
            Some(Headers(v)) => unimplemented!(),
            Some(frame) => panic!("unexpected frame; frame={:?}", frame),
            None => return Ok(Async::Ready(None)),
            _ => unimplemented!(),
        }
    }
}

impl<T, P> Sink for Connection<T, P>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
{
    type SinkItem = Frame<P::Send>;
    type SinkError = ConnectionError;

    fn start_send(&mut self, item: Self::SinkItem)
        -> StartSend<Self::SinkItem, Self::SinkError>
    {
        // First ensure that the upstream can process a new item
        if !try!(self.poll_ready()).is_ready() {
            return Ok(AsyncSink::NotReady(item));
        }

        match item {
            Frame::Message { id, message, body } => {
                // Ensure ID is valid
                try!(P::check_initiating_id(id));

                // TODO: Ensure available capacity for a new stream
                // This won't be as simple as self.streams.len() as closed
                // connections should not be factored.

                // Transition the stream state, creating a new entry if needed
                try!(self.streams.entry(id)
                     .or_insert(State::default())
                     .send_headers());

                let message = P::convert_send_message(id, message, body);

                // TODO: Handle trailers and all that jazz

                // We already ensured that the upstream can handle the frame, so
                // panic if it gets rejected.
                let res = try!(self.inner.start_send(frame::Frame::Headers(message.frame)));

                // This is a one-way conversion. By checking `poll_ready` first,
                // it's already been determined that the inner `Sink` can accept
                // the item. If the item is rejected, then there is a bug.
                assert!(res.is_ready());

                Ok(AsyncSink::Ready)
            }
            Frame::Body { id, chunk } => {
                unimplemented!();
            }
            Frame::Error { id, error } => {
                unimplemented!();
            }
        }
    }

    fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
        self.inner.poll_complete()
    }
}

impl<T, P> ReadySink for Connection<T, P>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
{
    fn poll_ready(&mut self) -> Poll<(), Self::SinkError> {
        self.inner.poll_ready()
    }
}
