use {ConnectionError, Frame, Peer};
use error;
use frame::{self, StreamId};
use client::Client;
use server::Server;

use proto::*;

use http::{request, response};
use bytes::{Bytes, IntoBuf};
use tokio_io::{AsyncRead, AsyncWrite};

use std::marker::PhantomData;

/// An H2 connection
#[derive(Debug)]
pub struct Connection<T, P, B: IntoBuf = Bytes> {
    // Codec
    codec: Codec<T, B::Buf>,

    // TODO: Remove <B>
    ping_pong: PingPong<B::Buf>,
    settings: Settings,
    streams: Streams,

    _phantom: PhantomData<P>,
}

/*
pub fn new<T, P, B>(transport: Transport<T, B::Buf>)
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
*/

impl<T, P, B> Connection<T, P, B>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
          B: IntoBuf,
{
    /// Polls for the next update to a remote flow control window.
    pub fn poll_window_update(&mut self) -> Poll<WindowUpdate, ConnectionError> {
        unimplemented!();
    }

    /// Increases the capacity of a local flow control window.
    pub fn expand_window(&mut self, id: StreamId, incr: WindowSize) -> Result<(), ConnectionError> {
        unimplemented!();
    }

    pub fn update_local_settings(&mut self, local: frame::SettingSet) -> Result<(), ConnectionError> {
        unimplemented!();
    }

    pub fn remote_initial_window_size(&self) -> u32 {
        unimplemented!();
    }

    pub fn remote_max_concurrent_streams(&self) -> Option<u32> {
        unimplemented!();
    }

    pub fn remote_push_enabled(&self) -> Option<bool> {
        unimplemented!();
    }

    pub fn start_ping(&mut self, body: PingPayload) -> StartSend<PingPayload, ConnectionError> {
        unimplemented!();
    }

    pub fn take_pong(&mut self) -> Option<PingPayload> {
        unimplemented!();
    }

    pub fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        unimplemented!();
    }

    pub fn send_data(self,
                     id: StreamId,
                     data: B,
                     end_of_stream: bool)
        -> sink::Send<Self>
    {
        unimplemented!();
    }

    // ===== Private =====

    /// Returns `Ready` when the `Connection` is ready to receive a frame from
    /// the socket.
    fn poll_recv_ready(&mut self) -> Poll<(), ConnectionError> {
        // `Connection` can only handle a single in-flight ping/pong. So, we
        // cannot read a new frame until the pending pong is written.
        //
        // This is also the highest priority frame to send.
        try_ready!(self.ping_pong.send_pongs(&mut self.codec));

        // Send any pending stream refusals
        try_ready!(self.streams.send_refuse(&mut self.codec));

        Ok(Async::Ready(()))
    }

    /// Try to receive the next frame
    fn recv_frame(&mut self) -> Poll<Option<Frame<P::Poll>>, ConnectionError> {
        use frame::Frame::*;

        loop {
            // First, ensure that the `Connection` is able to receive a frame
            try_ready!(self.poll_recv_ready());

            match try_ready!(self.codec.poll()) {
                Some(Headers(frame)) => {
                    if let Some(frame) = try!(self.streams.recv_headers(frame)) {
                        // Convert the frame
                        let frame = Frame::Headers {
                            id: frame.stream_id(),
                            end_of_stream: frame.is_end_stream(),
                            headers: P::convert_poll_message(frame),
                        };

                        return Ok(Some(frame).into());
                    }
                }
                Some(Data(frame)) => {
                    try!(self.streams.recv_data(&frame));

                    let frame = Frame::Data {
                        id: frame.stream_id(),
                        end_of_stream: frame.is_end_stream(),
                        data: frame.into_payload(),
                    };

                    return Ok(Some(frame).into());
                }
                Some(Reset(frame)) => {
                    try!(self.streams.recv_reset(&frame));

                    let frame = Frame::Reset {
                        id: frame.stream_id(),
                        error: frame.reason(),
                    };

                    return Ok(Some(frame).into());
                }
                Some(PushPromise(v)) => {
                    unimplemented!();
                }
                Some(Settings(v)) => {
                    self.settings.recv_settings(v);

                    // TODO: ACK must be sent THEN settings applied.
                }
                Some(Ping(v)) => {
                    self.ping_pong.recv_ping(v);

                    // TODO: Should poll_complete be called here? I don't think
                    // it should...
                }
                Some(WindowUpdate(v)) => {
                    unimplemented!();
                }
                None => return Ok(Async::Ready(None)),
            }
        }
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
        unimplemented!();
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
        unimplemented!();
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
        // TODO: intercept errors and flag the connection
        self.recv_frame()
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
        unimplemented!();
    }

    fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
        unimplemented!();
    }
}
