use {ConnectionError, Frame};
use client::Client;
use error;
use frame::{self, StreamId};
use proto::*;
use proto::ping_pong::{ControlPing, PingPayload};
use proto::settings::ControlSettings;
use server::Server;

use bytes::{Bytes, IntoBuf};
use http::{request, response};
use tokio_io::{AsyncRead, AsyncWrite};
use std::marker::PhantomData;

/// An H2 connection
#[derive(Debug)]
pub struct Connection<T, P, B: IntoBuf = Bytes> {
    inner: Transport<T, P, B::Buf>,
    // Set to `true` as long as the connection is in a valid state.
    active: bool,
    _phantom: PhantomData<(P, B)>,
}

pub fn new<T, P, B>(transport: Transport<T, P, B::Buf>)
    -> Connection<T, P, B>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
          B: IntoBuf,
{
    Connection {
        inner: transport,
        active: true,
        _phantom: PhantomData,
    }
}

impl<T, P, B> Connection<T, P, B>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
          B: IntoBuf,
{
    /// Polls for the next update to a remote flow control window.
    pub fn poll_window_update(&mut self) -> Poll<WindowUpdate, ConnectionError> {
        self.inner.poll_window_update()
    }

    /// Increases the capacity of a local flow control window.
    pub fn expand_window(&mut self, id: StreamId, incr: WindowSize) -> Result<(), ConnectionError> {
        self.inner.expand_window(id, incr)
    }

    pub fn update_local_settings(&mut self, local: frame::SettingSet) -> Result<(), ConnectionError> {
        self.inner.update_local_settings(local)
    }

    pub fn remote_initial_window_size(&self) -> u32 {
        self.inner.remote_initial_window_size()
    }

    pub fn remote_max_concurrent_streams(&self) -> Option<u32> {
        self.inner.remote_max_concurrent_streams()
    }

    pub fn remote_push_enabled(&self) -> Option<bool> {
        self.inner.remote_push_enabled()
    }

    pub fn start_ping(&mut self, body: PingPayload) -> StartSend<PingPayload, ConnectionError> {
        self.inner.start_ping(body)
    }

    pub fn take_pong(&mut self) -> Option<PingPayload> {
        self.inner.take_pong()
    }

    pub fn send_data(self,
                     id: StreamId,
                     data: B,
                     end_of_stream: bool)
        -> sink::Send<Self>
    {
        trace!("send_data: id={:?}", id);
        self.send(Frame::Data {
            id,
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

        if !self.active {
            return Err(error::User::Corrupt.into());
        }

        loop {
            let frame = match try!(self.inner.poll()) {
                Async::Ready(f) => f,

                // XXX is this necessary?
                Async::NotReady => {
                    // Receiving new frames may depend on ensuring that the write buffer
                    // is clear (e.g. if window updates need to be sent), so `poll_complete`
                    // is called here. 
                    try_ready!(self.poll_complete());

                    // If the write buffer is cleared, attempt to poll the underlying
                    // stream once more because it, may have been made ready.
                    try_ready!(self.inner.poll())
                }
            };

            trace!("poll; frame={:?}", frame);
            let frame = match frame {
                Some(Headers(v)) => Frame::Headers {
                    id: v.stream_id(),
                    end_of_stream: v.is_end_stream(),
                    headers: P::convert_poll_message(v),
                },

                Some(Data(v)) => Frame::Data {
                    id: v.stream_id(),
                    end_of_stream: v.is_end_stream(),
                    data: v.into_payload(),
                },

                Some(Reset(v)) => Frame::Reset {
                    id: v.stream_id(),
                    error: v.reason(),
                },

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
        trace!("start_send");

        if !self.active {
            return Err(error::User::Corrupt.into());
        }

        // Ensure the transport is ready to send a frame before we transform the external
        // `Frame` into an internal `frame::Frame`.
        if !try!(self.poll_ready()).is_ready() {
            return Ok(AsyncSink::NotReady(item));
        }

        let frame = match item {
            Frame::Headers { id, headers, end_of_stream } => {
                // This is a one-way conversion. By checking `poll_ready` first (above),
                // it's already been determined that the inner `Sink` can accept the item.
                // If the item is rejected, then there is a bug.
                let f = P::convert_send_message(id, headers, end_of_stream);
                frame::Frame::Headers(f)
            }

            Frame::Data { id, data, end_of_stream } => {
                frame::Data::from_buf(id, data.into_buf(), end_of_stream).into()
            }

            Frame::Reset { id, error } => frame::Reset::new(id, error).into(),

            /*
            Frame::Trailers { id, headers } => {
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
        };

        let res = self.inner.start_send(frame)?;
        assert!(res.is_ready());
        Ok(AsyncSink::Ready)
    }

    fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
        trace!("poll_complete");
        self.inner.poll_complete()
    }
}

impl<T, P, B> ReadySink for Connection<T, P, B>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
          B: IntoBuf,
{
    fn poll_ready(&mut self) -> Poll<(), Self::SinkError> {
        trace!("poll_ready");
        self.inner.poll_ready()
    }
}
