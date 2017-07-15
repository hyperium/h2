use {Frame, FrameSize};
use client::Client;
use error::{self, ConnectionError};
use frame::{self, StreamId};
use proto::{self, Peer, ReadySink, StreamState, FlowController, WindowSize};
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
    inner: proto::Transport<T, B::Buf>,
    streams: StreamMap<StreamState>,
    peer: PhantomData<P>,

    /// Tracks the connection-level flow control window for receiving data from the
    /// remote.
    local_flow_controller: FlowController,

    /// Tracks the onnection-level flow control window for receiving data from the remote.
    remote_flow_controller: FlowController,

    /// When `poll_window_update` is not ready, then the calling task is saved to be
    /// notified later. Access to poll_window_update must not be shared across tasks.
    blocked_window_update: Option<task::Task>,

    sending_window_update: Option<frame::WindowUpdate>,
}

type StreamMap<T> = OrderMap<StreamId, T, BuildHasherDefault<FnvHasher>>;

pub fn new<T, P, B>(transport: proto::Transport<T, B::Buf>)
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

        local_flow_controller: FlowController::new(recv_window_size),
        remote_flow_controller: FlowController::new(send_window_size),

        blocked_window_update: None,
        sending_window_update: None,
    }
}

impl<T, P, B: IntoBuf> Connection<T, P, B> {
    #[inline]
    fn claim_local_window(&mut self, len: WindowSize) -> Result<(), ConnectionError> {
        self.local_flow_controller.claim_window(len)
            .map_err(|_| error::Reason::FlowControlError.into())
    }

    #[inline]
    fn claim_remote_window(&mut self, len: WindowSize) -> Result<(), ConnectionError> {
        self.remote_flow_controller.claim_window(len)
            .map_err(|_| error::User::FlowControlViolation.into())
    }

    /// Polls for the amount of additional data that may be sent to a remote.
    ///
    /// Connection  and stream updates are distinct.
    pub fn poll_window_update(&mut self, id: StreamId) -> Poll<WindowSize, ConnectionError> {
        let added = if id.is_zero() {
            self.remote_flow_controller.take_window_update()
        } else {
            self.streams.get_mut(&id).and_then(|s| s.take_send_window_update())
        };

        match added {
            Some(incr) => Ok(Async::Ready(incr)),
            None => {
                self.blocked_window_update = Some(task::current());
                Ok(Async::NotReady)
            }
        }
    }


    /// Increases the amount of data that the remote endpoint may send.
    ///
    /// Connection and stream updates are distinct.
    pub fn increment_window_size(&mut self, id: StreamId, incr: WindowSize) {
        assert!(self.sending_window_update.is_none());

        let added = if id.is_zero() {
            self.local_flow_controller.grow_window(incr);
            self.local_flow_controller.take_window_update()
        } else {
            self.streams.get_mut(&id).and_then(|s| {
                s.grow_recv_window(incr);
                s.take_recv_window_update()
            })
        };

        if let Some(added) = added {
            self.sending_window_update = Some(frame::WindowUpdate::new(id, added));
        }
    }

    /// Handles a window update received from the remote, indicating that the local may
    /// send `incr` additional bytes.
    ///
    /// Connection window updates (id=0) and stream window updates are advertised
    /// distinctly.
    fn increment_send_window_size(&mut self, id: StreamId, incr: WindowSize) {
        if incr == 0 {
            return;
        }

        let added = if id.is_zero() {
            self.remote_flow_controller.grow_window(incr);
            true
        } else if let Some(mut s) = self.streams.get_mut(&id) {
            s.grow_send_window(incr);
            true
        } else {
            false
        };

        if added {
            if let Some(task) = self.blocked_window_update.take() {
                task.notify();
            }
        }
    }
}

impl<T, P, B> Connection<T, P, B>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
          B: IntoBuf
{
    /// Attempts to send a window update to the remote, if one is pending.
    fn poll_sending_window_update(&mut self) -> Poll<(), ConnectionError> {
        if let Some(f) = self.sending_window_update.take() {
            if self.inner.start_send(f.into())?.is_not_ready() {
                self.sending_window_update = Some(f);
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
        trace!("poll");

        loop {
            let frame = match try!(self.inner.poll()) {
                Async::Ready(f) => f,
                Async::NotReady => {
                    // Receiving new frames may depend on ensuring that the write buffer
                    // is clear (e.g. if window updates need to be sent), so `poll_complete`
                    // is called here. 
                    try_ready!(self.inner.poll_complete());

                    // If the sender sink is ready, we attempt to poll the underlying
                    // stream once more because it, may have been made ready by flushing
                    // the sink.
                    try_ready!(self.inner.poll())
                }
            };

            trace!("poll; frame={:?}", frame);
            let frame = match frame {
                Some(Headers(v)) => {
                    // TODO: Update stream state
                    let stream_id = v.stream_id();
                    let end_of_stream = v.is_end_stream();

                    let init_window_size = self.inner.local_settings().initial_window_size();

                    let stream_initialized = try!(self.streams.entry(stream_id)
                        .or_insert(StreamState::default())
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

                    self.claim_local_window(v.len())?;
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
                    // When a window update is received from the remote, apply that update
                    // to the proper stream so that more data may be sent to the remote.
                    self.increment_send_window_size(v.stream_id(), v.size_increment());

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
        trace!("start_send");

        // Ensure that a pending window update is sent before doing anything further and
        // ensure that the inner sink will actually receive a frame.
        if self.poll_ready()? == Async::NotReady {
            return Ok(AsyncSink::NotReady(item));
        }
        assert!(self.sending_window_update.is_none());

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
                     .or_insert(StreamState::default())
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
                try!(self.claim_remote_window(data_len));

                // The stream must be initialized at this point.
                match self.streams.get_mut(&id) {
                    None => return Err(error::User::InactiveStreamId.into()),
                    Some(mut s) => try!(s.send_data(end_of_stream, data_len)),
                }

                let mut frame = frame::Data::from_buf(id, data.into_buf());
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
        trace!("poll_complete");

        try_ready!(self.inner.poll_complete());

        // TODO check for settings updates and update the initial window size of all
        // streams.

        self.poll_sending_window_update()
    }
}

impl<T, P, B> ReadySink for Connection<T, P, B>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
          B: IntoBuf,
{
    fn poll_ready(&mut self) -> Poll<(), Self::SinkError> {
        trace!("poll_ready");
        try_ready!(self.inner.poll_ready());
        self.poll_sending_window_update()
    }
}
