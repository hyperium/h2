use {Frame, ConnectionError, Peer, StreamId};
use client::Client;
use frame::{Frame as WireFrame};
use server::Server;
use proto::{self, ReadySink, State, WindowUpdate};

use tokio_io::{AsyncRead, AsyncWrite};

use http::{request, response};

use futures::*;

use ordermap::OrderMap;
use fnv::FnvHasher;

use std::marker::PhantomData;
use std::hash::BuildHasherDefault;

pub struct FlowControlViolation;

#[derive(Debug)]
struct FlowController {
    window_size: u32,
    underflow: u32,
}

impl FlowController {
    pub fn new(window_size: u32) -> FlowController {
        FlowController {
            window_size,
            underflow: 0,
        }
    }

    pub fn shrink(&mut self, mut sz: u32) {
        self.underflow += sz;
    }

    pub fn consume(&mut self, mut sz: u32) -> Result<(), FlowControlViolation> {
        if sz < self.window_size {
            self.underflow -= sz;
            return Err(FlowControlViolation);
        }

        self.window_size -= sz;
        Ok(())
    }

    pub fn increment(&mut self, mut sz: u32) {
        if sz <= self.underflow {
            self.underflow -= sz;
            return;
        }

        sz -= self.underflow;
        self.window_size += sz;
    }
}

/// An H2 connection
#[derive(Debug)]
pub struct Connection<T, P> {
    inner: proto::Inner<T>,
    streams: StreamMap<State>,
    peer: PhantomData<P>,
    local_flow_controller: FlowController,
        remote_flow_controller: FlowController,
}

type StreamMap<T> = OrderMap<StreamId, T, BuildHasherDefault<FnvHasher>>;

pub fn new<T, P>(transport: proto::Inner<T>, initial_local_window_size: u32, initial_remote_window_size: u32) -> Connection<T, P>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
{
    Connection {
        inner: transport,
        streams: StreamMap::default(),
        peer: PhantomData,
        local_flow_controller: FlowController::new(initial_local_window_size),
        remote_flow_controller: FlowController::new(initial_remote_window_size),
    }
}

impl<T, P> Connection<T, P> {
    /// Publishes stream window updates to the remote.
    ///
    /// Connection window updates (StreamId=0) and stream window updates are published
    /// distinctly.
    pub fn increment_local_window(&mut self, up: WindowUpdate) {
        let incr = up.increment();
        let flow = match up {
            WindowUpdate::Connection { .. } => Some(&self.local_flow_controller),
            WindowUpdate::Stream { id, .. } => {
                self.streams.get(&id).map(|s| s.local_flow_controller())
            }
        };
        if let Some(flow) = flow {
            flow.increment(incr);
        }
        unimplemented!()
    }

    /// Advertises stream window updates from the remote.
    ///
    /// Connection window updates (StreamId=0) and stream window updates are advertised
    /// distinctly.
    pub fn poll_remote_window(&mut self) -> Poll<WindowUpdate, ()> {
        unimplemented!()
    }
}

impl<T> Connection<T, Client>
    where T: AsyncRead + AsyncWrite,
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

impl<T> Connection<T, Server>
    where T: AsyncRead + AsyncWrite,
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

impl<T, P> Stream for Connection<T, P>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
{
    type Item = Frame<P::Poll>;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Option<Self::Item>, ConnectionError> {
        trace!("Connection::poll");

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
            Some(WireFrame::Headers(v)) => {
                // TODO: Update stream state
                let stream_id = v.stream_id();
                let end_of_stream = v.is_end_stream();

                let stream_initialized = try!(self.streams.entry(stream_id)
                     .or_insert(State::default())
                     .recv_headers::<P>(end_of_stream));

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
            Some(WireFrame::Data(v)) => {
                // TODO: Validate frame

                let stream_id = v.stream_id();
                let end_of_stream = v.is_end_stream();

                Frame::Body {
                    id: stream_id,
                    chunk: v.into_payload(),
                    end_of_stream: end_of_stream,
                }
            }
            Some(frame) => panic!("unexpected frame; frame={:?}", frame),
            None => return Ok(Async::Ready(None)),
        };

        Ok(Async::Ready(Some(frame)))
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
            Frame::Headers { id, headers, end_of_stream } => {
                // Transition the stream state, creating a new entry if needed
                //
                // TODO: Response can send multiple headers frames before body
                // (1xx responses).
                let stream_initialized = try!(self.streams.entry(id)
                     .or_insert(State::default())
                     .send_headers::<P>(end_of_stream));

                if stream_initialized {
                    // TODO: Ensure available capacity for a new stream
                    // This won't be as simple as self.streams.len() as closed
                    // connections should not be factored.
                    //
                    if !P::is_valid_local_stream_id(id) {
                        unimplemented!();
                    }
                }

                let frame = P::convert_send_message(id, headers, end_of_stream);

                // We already ensured that the upstream can handle the frame, so
                // panic if it gets rejected.
                let res = try!(self.inner.start_send(WireFrame::Headers(frame)));

                // This is a one-way conversion. By checking `poll_ready` first,
                // it's already been determined that the inner `Sink` can accept
                // the item. If the item is rejected, then there is a bug.
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

impl<T, P> ReadySink for Connection<T, P>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
{
    fn poll_ready(&mut self) -> Poll<(), Self::SinkError> {
        self.inner.poll_ready()
    }
}
