use Frame;
use client::Client;
use error::{self, ConnectionError};
use frame::{self, StreamId};
use proto::{self, Peer, ReadySink, State, PeerState, WindowUpdate, FlowController};
use server::Server;

use tokio_io::{AsyncRead, AsyncWrite};

use http::{request, response};
use bytes::{Bytes, IntoBuf};

use futures::*;

use ordermap::OrderMap;
use fnv::FnvHasher;

use std::collections::VecDeque;
use std::hash::BuildHasherDefault;
use std::marker::PhantomData;

// TODO get window size from `inner`.

/// An H2 connection
#[derive(Debug)]
pub struct Connection<T, P, B: IntoBuf = Bytes> {
    inner: proto::Inner<T, B::Buf>,
    streams: StreamMap<State>,
    peer: PhantomData<P>,

    /// Tracks connection-level flow control.
    local_flow_controller: FlowController,
    initial_local_window_size: u32,
    pending_local_window_updates: VecDeque<WindowUpdate>,

    remote_flow_controller: FlowController,
    initial_remote_window_size: u32,
    pending_remote_window_updates: VecDeque<WindowUpdate>,
    blocked_remote_window_update: Option<task::Task>
}

type StreamMap<T> = OrderMap<StreamId, T, BuildHasherDefault<FnvHasher>>;

pub fn new<T, P, B>(transport: proto::Inner<T, B::Buf>,
                 initial_local_window_size: u32,
                 initial_remote_window_size: u32)
    -> Connection<T, P, B>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
          B: IntoBuf,
{
    Connection {
        inner: transport,
        streams: StreamMap::default(),
        peer: PhantomData,

        local_flow_controller: FlowController::new(initial_local_window_size),
        initial_local_window_size,
        pending_local_window_updates: VecDeque::default(),

        remote_flow_controller: FlowController::new(initial_remote_window_size),
        initial_remote_window_size,
        pending_remote_window_updates: VecDeque::default(),
        blocked_remote_window_update: None,
    }
}

impl<T, P, B: IntoBuf> Connection<T, P, B> {

    /// Publishes local stream window updates to the remote.
    ///
    /// Connection window updates (StreamId=0) and stream window must be published
    /// distinctly.
    pub fn increment_local_window(&mut self, up: WindowUpdate) {
        let added = match &up {
            &WindowUpdate::Connection { increment } => {
                if increment == 0 {
                    false
                } else {
                    self.local_flow_controller.increment(increment);
                    true
                }
            }
            &WindowUpdate::Stream { id, increment } => {
                if increment == 0 {
                    false
                } else {
                    match self.streams.get_mut(&id) {
                        Some(&mut State::Open { local: PeerState::Data(ref mut fc), .. }) |
                        Some(&mut State::HalfClosedRemote(PeerState::Data(ref mut fc))) => {
                            fc.increment(increment);
                            true
                        }
                        _ => false,
                    }
                }
            }
        };

        if added {
            self.pending_local_window_updates.push_back(up);
        }
    }

    /// Advertises the remote's stream window updates.
    ///
    /// Connection window updates (StreamId=0) and stream window updates are advertised
    /// distinctly.
    fn increment_remote_window(&mut self, id: StreamId, incr: u32) {
        if id.is_zero() {
            self.remote_flow_controller.increment(incr);
        } else {
            match self.streams.get_mut(&id) {
                Some(&mut State::Open { remote: PeerState::Data(ref mut fc), .. }) |
                Some(&mut State::HalfClosedLocal(PeerState::Data(ref mut fc))) => {
                    fc.increment(incr);
                }
                _ => {}
            }
        }
        unimplemented!()
    }
}

impl<T, P, B> Connection<T, P, B>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
          B: IntoBuf,
{
    pub fn send_data(self,
                     id: StreamId,
                     data: B,
                     end_of_stream: bool)
        -> sink::Send<Self>
    {
        self.send(Frame::Data {
            id: id,
            data: data,
            end_of_stream: end_of_stream,
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
                    // Because receiving new frames may depend on ensuring that the
                    // write buffer is clear, `poll_complete` is called here.
                    let _ = try!(self.poll_complete());
                    return Ok(Async::NotReady);
                }
            };

            trace!("received; frame={:?}", frame);

            let frame = match frame {
                Some(Headers(v)) => {
                    // TODO: Update stream state
                    let stream_id = v.stream_id();
                    let end_of_stream = v.is_end_stream();

                    // TODO load window size from settings.
                    let init_window_size = 65_535;

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
                    // TODO: Validate frame

                    let stream_id = v.stream_id();
                    let end_of_stream = v.is_end_stream();
                    match self.streams.get_mut(&stream_id) {
                        None => return Err(error::Reason::ProtocolError.into()),
                        Some(state) => try!(state.recv_data(end_of_stream)),
                    }

                    Frame::Data {
                        id: stream_id,
                        data: v.into_payload(),
                        end_of_stream: end_of_stream,
                    }
                }
                Some(WindowUpdate(v)) => {
                    self.increment_remote_window(v.stream_id(), v.size_increment());
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

    fn start_send(&mut self, item: Self::SinkItem)
        -> StartSend<Self::SinkItem, Self::SinkError>
    {
        use frame::Frame::Headers;

        // First ensure that the upstream can process a new item
        if !try!(self.poll_ready()).is_ready() {
            return Ok(AsyncSink::NotReady(item));
        }

        match item {
            Frame::Headers { id, headers, end_of_stream } => {
                // TODO load window size from settings.
                let init_window_size = 65_535;

                // Transition the stream state, creating a new entry if needed
                // TODO: Response can send multiple headers frames before body
                // (1xx responses).
                let stream_initialized = try!(self.streams.entry(id)
                     .or_insert(State::default())
                     .send_headers::<P>(end_of_stream, init_window_size));

                if stream_initialized {
                    // TODO: Ensure available capacity for a new stream
                    // This won't be as simple as self.streams.len() as closed
                    // connections should not be factored.
                    //
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
            Frame::Data { id, data, end_of_stream } => {
                // The stream must be initialized at this point
                match self.streams.get_mut(&id) {
                    None => return Err(error::User::InactiveStreamId.into()),
                    Some(state) => try!(state.send_data(end_of_stream)),
                }

                let mut frame = frame::Data::new(id, data.into_buf());

                if end_of_stream {
                    frame.set_end_stream();
                }

                let res = try!(self.inner.start_send(frame.into()));

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
        self.inner.poll_complete()
    }
}

impl<T, P, B> ReadySink for Connection<T, P, B>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
          B: IntoBuf,
{
    fn poll_ready(&mut self) -> Poll<(), Self::SinkError> {
        self.inner.poll_ready()
    }
}
