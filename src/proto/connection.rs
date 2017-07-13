use {Frame, FrameSize};
use client::Client;
use error::{self, ConnectionError};
use frame::{self, StreamId};
use proto::{self, Peer, ReadySink, State, FlowController, WindowSize};
use server::Server;

use tokio_io::{AsyncRead, AsyncWrite};

use http::{request, response};
use bytes::{Bytes, IntoBuf};

use futures::*;

use ordermap::OrderMap;
use fnv::FnvHasher;

use std::hash::BuildHasherDefault;
use std::marker::PhantomData;

/// An H2 connection
#[derive(Debug)]
pub struct Connection<T, P, B: IntoBuf = Bytes> {
    inner: proto::Inner<T, B::Buf>,
    streams: StreamMap<State>,
    peer: PhantomData<P>,

    /// Tracks connection-level flow control.
    recv_flow_controller: FlowController,
    send_flow_controller: FlowController,


    pending_send_window_update: Option<frame::WindowUpdate>,
    blocked_recv_window_update: Option<task::Task>,
}

type StreamMap<T> = OrderMap<StreamId, T, BuildHasherDefault<FnvHasher>>;

pub fn new<T, P, B>(transport: proto::Inner<T, B::Buf>)
    -> Connection<T, P, B>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
          B: IntoBuf,
{
    let recv_window_size = transport.local_settings().initial_window_size();
    let send_window_size = transport.remote_settings().initial_window_size();
    Connection {
        inner: transport,
        streams: StreamMap::default(),
        peer: PhantomData,

        recv_flow_controller: FlowController::new(recv_window_size),
        send_flow_controller: FlowController::new(send_window_size),

        pending_send_window_update: None,
        blocked_recv_window_update: None,
    }
}

impl<T, P, B: IntoBuf> Connection<T, P, B> {
    #[inline]
    fn claim_connection_recv_window(&mut self, len: WindowSize) -> Result<(), ConnectionError> {
        self.recv_flow_controller.claim_window(len)
            .map_err(|_| error::Reason::FlowControlError.into())
    }

    #[inline]
    fn claim_connection_send_window(&mut self, len: WindowSize) -> Result<(), ConnectionError> {
        self.send_flow_controller.claim_window(len)
            .map_err(|_| error::Reason::FlowControlError.into())
    }

    // TODO check max frame size

    pub fn poll_remote_window_update(&mut self, id: StreamId) -> Poll<WindowSize, ConnectionError> {
        let added = if id.is_zero() {
            self.send_flow_controller.take_window_update()
        } else {
            self.streams.get_mut(&id).and_then(|mut s| s.take_recv_window_update())
        };

        match added {
            Some(incr) => Ok(Async::Ready(incr)),
            None => {
                self.blocked_recv_window_update = Some(task::current());
                Ok(Async::NotReady)
            }
        }
    }


    /// Publishes local stream window updates to the remote.
    ///
    /// Connection window updates (StreamId=0) and stream window must be published
    /// distinctly.
    pub fn init_send_window_update(&mut self, id: StreamId, incr: WindowSize) {
        assert!(self.pending_send_window_update.is_none());

        let added = if id.is_zero() {
            self.send_flow_controller.add_to_window(incr);
            self.send_flow_controller.take_window_update()
        } else {
            self.streams.get_mut(&id).and_then(|mut s| s.send_window_update(incr))
        };

        if let Some(added) = added {
            self.pending_send_window_update = Some(frame::WindowUpdate::new(id, added));
        }
    }

    /// Advertises the remote's stream window updates.
    ///
    /// Connection window updates (id=0) and stream window updates are advertised
    /// distinctly.
    fn recv_window_update(&mut self, id: StreamId, incr: WindowSize) {
        if id.is_zero() {
            return self.recv_flow_controller.add_to_window(incr);
        }

        if let Some(mut s) = self.streams.get_mut(&id) {
            s.recv_window_update(incr);
        }
    }
}

impl<T, P, B> Connection<T, P, B>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
          B: IntoBuf
{
    /// Attempts to send a window update to the remote, if one is pending.
    fn poll_send_window_update(&mut self) -> Poll<(), ConnectionError> {
        if let Some(f) = self.pending_send_window_update.take() {
            if self.inner.start_send(f.into())?.is_not_ready() {
                self.pending_send_window_update = Some(f);
                return Ok(Async::NotReady);
            }
        }
        Ok(Async::Ready(()))
    }
}

// Note: this is bytes-specific for now so that we can know the payload's length.
impl<T, P> Connection<T, P, Bytes>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
{
    pub fn send_data(self,
                     id: StreamId,
                     data: Bytes,
                     end_of_stream: bool)
        -> sink::Send<Self>
    {
        self.send(Frame::Data {
            id,
            data_len: data.len() as FrameSize,
            data,
            end_of_stream,
        })
    }
}

impl<T, B> Connection<T, Client, B>
    where T: AsyncRead + AsyncWrite,
          B: IntoBuf,
{
    pub fn send_request(self,
                        id: StreamId, // TODO: Generate one internally?
                        request: request::Head,
                        end_of_stream: bool)
        -> sink::Send<Self>
    {
        self.send(Frame::Headers {
            id: id,
            headers: request,
            end_of_stream: end_of_stream,
        })
    }
}

impl<T, B> Connection<T, Server, B>
    where T: AsyncRead + AsyncWrite,
          B: IntoBuf,
{
    pub fn send_response(self,
                        id: StreamId, // TODO: Generate one internally?
                        response: response::Head,
                        end_of_stream: bool)
        -> sink::Send<Self>
    {
        self.send(Frame::Headers {
            id: id,
            headers: response,
            end_of_stream: end_of_stream,
        })
    }
}

impl<T, P, B> Stream for Connection<T, P, B>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
          B: IntoBuf,
{
    type Item = Frame<P::Poll>;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Option<Self::Item>, ConnectionError> {
        use frame::Frame::*;
        trace!("Connection::poll");

        loop {
            let frame = match try!(self.inner.poll()) {
                Async::Ready(f) => f,
                Async::NotReady => {
                    // Receiving new frames may depend on ensuring that the write buffer
                    // is clear (e.g. if window updates need to be sent), so `poll_ready`
                    // is called here. 
                    try_ready!(self.poll_ready());

                    // If the snder sink is ready, we attempt to poll the underlying
                    // stream once more because it, may have been made ready by flushing
                    // the sink.
                    try_ready!(self.inner.poll())
                }
            };

            trace!("received; frame={:?}", frame);
            let frame = match frame {
                Some(Headers(v)) => {
                    // TODO: Update stream state
                    let stream_id = v.stream_id();
                    let end_of_stream = v.is_end_stream();

                    let init_window_size = self.inner.local_settings().initial_window_size();

                    let stream_initialized = try!(self.streams.entry(stream_id)
                        .or_insert(State::default())
                        .recv_headers::<P>(end_of_stream, init_window_size));

                    if stream_initialized {
                        // TODO: Ensure available capacity for a new stream
                        // This won't be as simple as self.streams.len() as closed
                        // connections should not be factored.

                        if !P::is_valid_remote_stream_id(stream_id) {
                            unimplemented!();
                        }
                    }

                    Frame::Headers {
                        id: stream_id,
                        headers: P::convert_poll_message(v),
                        end_of_stream: end_of_stream,
                    }
                }

                Some(Data(v)) => {
                    let id = v.stream_id();
                    let end_of_stream = v.is_end_stream();

                    self.claim_connection_recv_window(v.len())?;
                    match self.streams.get_mut(&id) {
                        None => return Err(error::Reason::ProtocolError.into()),
                        Some(state) => state.recv_data(end_of_stream, v.len())?,
                    }

                    Frame::Data {
                        id,
                        end_of_stream,
                        data_len: v.len(),
                        data: v.into_payload(),
                    }
                }

                Some(WindowUpdate(v)) => {
                    // When a window update is read from the remote, apply that update to
                    // the proper stream.
                    self.recv_window_update(v.stream_id(), v.size_increment());

                    // There's nothing to return yet, so continue attempting to read
                    // additional frames.
                    continue;
                }

                Some(frame) => panic!("unexpected frame; frame={:?}", frame),
                None => return Ok(Async::Ready(None)),
            };

            return Ok(Async::Ready(Some(frame)));
        }
    }
}

impl<T, P, B> Sink for Connection<T, P, B>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
          B: IntoBuf,
{
    type SinkItem = Frame<P::Send, B>;
    type SinkError = ConnectionError;

    /// Sends a frame to the remote.
    fn start_send(&mut self, item: Self::SinkItem)
        -> StartSend<Self::SinkItem, Self::SinkError>
    {
        use frame::Frame::Headers;
        trace!("Connection::start_send");

        // First ensure that the upstream can process a new item. This ensures, for
        // instance, that any pending local window updates have been sent to the remote
        // before sending any other (i.e. DATA) frames.
        if try!(self.poll_ready()).is_not_ready() {
            return Ok(AsyncSink::NotReady(item));
        }
        assert!(self.pending_send_window_update.is_none());

        match item {
            Frame::Headers { id, headers, end_of_stream } => {
                let init_window_size = self.inner.remote_settings().initial_window_size();

                // Transition the stream state, creating a new entry if needed
                //
                // TODO: Response can send multiple headers frames before body (1xx
                // responses).
                //
                // ACTUALLY(ver), maybe not?
                //   https://github.com/http2/http2-spec/commit/c83c8d911e6b6226269877e446a5cad8db921784
                let stream_initialized = try!(self.streams.entry(id)
                     .or_insert(State::default())
                     .send_headers::<P>(end_of_stream, init_window_size));

                if stream_initialized {
                    // TODO: Ensure available capacity for a new stream
                    // This won't be as simple as self.streams.len() as closed
                    // connections should not be factored.
                    if !P::is_valid_local_stream_id(id) {
                        // TODO: clear state
                        return Err(error::User::InvalidStreamId.into());
                    }
                }

                let frame = P::convert_send_message(id, headers, end_of_stream);

                // We already ensured that the upstream can handle the frame, so
                // panic if it gets rejected.
                let res = try!(self.inner.start_send(Headers(frame)));

                // This is a one-way conversion. By checking `poll_ready` first,
                // it's already been determined that the inner `Sink` can accept
                // the item. If the item is rejected, then there is a bug.
                assert!(res.is_ready());

                Ok(AsyncSink::Ready)
            }

            Frame::Data { id, data, data_len, end_of_stream } => {
                self.claim_connection_send_window(data_len)?;

                // The stream must be initialized at this point.
                match self.streams.get_mut(&id) {
                    None => return Err(error::User::InactiveStreamId.into()),
                    Some(state) => try!(state.send_data(end_of_stream, data_len)),
                }

                let mut frame = frame::Data::from_buf(id, data.into_buf());
                if end_of_stream {
                    frame.set_end_stream();
                }

                let res = try!(self.inner.start_send(frame.into()));

                // poll_ready has already been called.
                assert!(res.is_ready());

                Ok(AsyncSink::Ready)
            }

            /*
            Frame::Trailers { id, headers } => {
                unimplemented!();
            }
            Frame::Body { id, chunk, end_of_stream } => {
                unimplemented!();
            }
            Frame::PushPromise { id, promise } => {
                unimplemented!();
            }
            Frame::Error { id, error } => {
                unimplemented!();
            }
            */
            _ => unimplemented!(),
        }
    }

    fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
        trace!("Connection::poll_complete");
        try_ready!(self.inner.poll_complete());
        self.poll_send_window_update()
    }
}

impl<T, P, B> ReadySink for Connection<T, P, B>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
          B: IntoBuf,
{
    fn poll_ready(&mut self) -> Poll<(), Self::SinkError> {
        trace!("Connection::poll_ready");
        try_ready!(self.inner.poll_ready());
        self.poll_send_window_update()
    }
}
