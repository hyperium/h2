use {ConnectionError, Frame, FrameSize};
use client::Client;
use frame::{self, SettingSet, StreamId};
use proto::{self, ControlSettings, Peer, ReadySink, ControlFlow, WindowSize};
use server::Server;

use tokio_io::{AsyncRead, AsyncWrite};

use http::{request, response};
use bytes::{Bytes, IntoBuf};

use futures::*;

use std::marker::PhantomData;

/// An H2 connection
#[derive(Debug)]
pub struct Connection<T, P, B: IntoBuf = Bytes> {
    inner: proto::Transport<T, P, B::Buf>,
    peer: PhantomData<P>,
}

pub fn new<T, P, B>(transport: proto::Transport<T, P, B::Buf>)
    -> Connection<T, P, B>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
          B: IntoBuf,
{
    Connection {
        inner: transport,
        peer: PhantomData,
    }
}


impl<T, P, B> ControlSettings for Connection<T, P, B>
    where T: ControlSettings,
          B: IntoBuf,
{
    fn update_local_settings(&mut self, local: frame::SettingSet) -> Result<(), ConnectionError> {
        self.inner.update_local_settings(local)
    }

    fn local_settings(&self) -> &SettingSet {
        self.inner.local_settings()
    }

    fn remote_settings(&self) -> &SettingSet {
        self.inner.remote_settings()
    }
}

impl<T, P, B> ControlFlow for Connection<T, P, B>
    where T: ControlFlow,
          B: IntoBuf,
{
    fn poll_remote_window_update(&mut self, id: StreamId) -> Poll<WindowSize, ConnectionError> {
        self.inner.poll_remote_window_update(id)
    }

    fn grow_local_window(&mut self, id: StreamId, incr: WindowSize) -> Result<(), ConnectionError> {
        self.inner.grow_local_window(id, incr)
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

                    // If the write buffer is cleared, attempt to poll the underlying
                    // stream once more because it, may have been made ready.
                    try_ready!(self.inner.poll())
                }
            };

            trace!("poll; frame={:?}", frame);
            let frame = match frame {
                Some(Headers(v)) => {
                    // TODO: Update stream state
                    let stream_id = v.stream_id();
                    let end_of_stream = v.is_end_stream();

                    Frame::Headers {
                        id: stream_id,
                        headers: P::convert_poll_message(v),
                        end_of_stream: end_of_stream,
                    }
                }

                Some(Data(v)) => {
                    let id = v.stream_id();
                    let end_of_stream = v.is_end_stream();

                    Frame::Data {
                        id,
                        end_of_stream,
                        data_len: v.len(),
                        data: v.into_payload(),
                    }
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

        match item {
            Frame::Headers { id, headers, end_of_stream } => {
                // This is a one-way conversion. By checking `poll_ready` first (above),
                // it's already been determined that the inner `Sink` can accept the item.
                // If the item is rejected, then there is a bug.
                let frame = P::convert_send_message(id, headers, end_of_stream);
                let res = self.inner.start_send(Headers(frame))?;
                assert!(res.is_ready());
                Ok(AsyncSink::Ready)
            }

            Frame::Data { id, data, end_of_stream, .. } => {
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
