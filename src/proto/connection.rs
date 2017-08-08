use {client, ConnectionError, Frame};
use HeaderMap;
use frame::{self, StreamId};

use proto::*;

use http::{Request, Response};
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
    streams: Streams<P, B::Buf>,

    _phantom: PhantomData<P>,
}

impl<T, P, B> Connection<T, P, B>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
          B: IntoBuf,
{
    pub fn new(codec: Codec<T, B::Buf>) -> Connection<T, P, B> {
        // TODO: Actually configure
        let streams = Streams::new(streams::Config {
            max_remote_initiated: None,
            init_remote_window_sz: DEFAULT_INITIAL_WINDOW_SIZE,
            max_local_initiated: None,
            init_local_window_sz: DEFAULT_INITIAL_WINDOW_SIZE,
        });

        Connection {
            codec: codec,
            ping_pong: PingPong::new(),
            settings: Settings::new(),
            streams: streams,
            _phantom: PhantomData,
        }
    }

    /// Polls for the next update to a remote flow control window.
    pub fn poll_window_update(&mut self) -> Poll<WindowUpdate, ConnectionError> {
        self.streams.poll_window_update()
    }

    /// Increases the capacity of a local flow control window.
    ///
    /// # Panics
    ///
    /// THis function panics if `incr` is not a valid window size.
    pub fn expand_window(&mut self, id: StreamId, incr: usize)
    -> Result<(), ConnectionError>
    {
        assert!(incr <= MAX_WINDOW_SIZE as usize);
        self.streams.expand_window(id, incr as WindowSize)
    }

    pub fn update_local_settings(&mut self, _local: frame::SettingSet) -> Result<(), ConnectionError> {
        unimplemented!();
    }

    pub fn remote_initial_window_size(&self) -> u32 {
        unimplemented!();
    }

    pub fn remote_max_concurrent_streams(&self) -> Option<usize> {
        unimplemented!();
    }

    pub fn remote_push_enabled(&self) -> Option<bool> {
        unimplemented!();
    }

    /// Returns `Ready` when the connection is ready to receive a frame.
    pub fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        try_ready!(self.poll_send_ready());

        // TODO: Once there is write buffering, this shouldn't be needed
        try_ready!(self.codec.poll_ready());

        Ok(().into())
    }

    /// Advances the internal state of the connection.
    pub fn poll(&mut self) -> Poll<(), ConnectionError> {
        match self.poll2() {
            Err(e) => {
                self.streams.recv_err(&e);
                Err(e)
            }
            ret => ret,
        }
    }

    fn poll2(&mut self) -> Poll<(), ConnectionError> {
        use frame::Frame::*;

        loop {
            // First, ensure that the `Connection` is able to receive a frame
            try_ready!(self.poll_recv_ready());

            trace!("polling codec");

            let frame = match try!(self.codec.poll()) {
                Async::Ready(frame) => frame,
                Async::NotReady => {
                    // Flush any pending writes
                    let _ = try!(self.poll_complete());
                    return Ok(Async::NotReady);
                }
            };

            match frame {
                Some(Headers(frame)) => {
                    trace!("recv HEADERS; frame={:?}", frame);

                    if let Some(frame) = try!(self.streams.recv_headers(frame)) {
                        unimplemented!();
                    }

                    /*
                    // Update stream state while ensuring that the headers frame
                    // can be received.
                    if let Some(frame) = try!(self.streams.recv_headers(frame)) {
                        let frame = Self::convert_poll_message(frame)?;
                        return Ok(Some(frame).into());
                    }
                    */
                }
                Some(Data(frame)) => {
                    trace!("recv DATA; frame={:?}", frame);
                    try!(self.streams.recv_data(frame));
                }
                Some(Reset(frame)) => {
                    unimplemented!();
                    /*
                    trace!("recv RST_STREAM; frame={:?}", frame);
                    try!(self.streams.recv_reset(&frame));

                    let frame = Frame::Reset {
                        id: frame.stream_id(),
                        error: frame.reason(),
                    };

                    return Ok(Some(frame).into());
                    */
                }
                Some(PushPromise(frame)) => {
                    // TODO: implement
                    /*
                    trace!("recv PUSH_PROMISE; frame={:?}", frame);
                    try!(self.streams.recv_push_promise(frame));
                    */
                }
                Some(Settings(frame)) => {
                    trace!("recv SETTINGS; frame={:?}", frame);
                    self.settings.recv_settings(frame);

                    // TODO: ACK must be sent THEN settings applied.
                }
                Some(Ping(frame)) => {
                    unimplemented!();
                    /*
                    trace!("recv PING; frame={:?}", frame);
                    self.ping_pong.recv_ping(frame);
                    */
                }
                Some(WindowUpdate(frame)) => {
                    unimplemented!();
                    /*
                    trace!("recv WINDOW_UPDATE; frame={:?}", frame);
                    try!(self.streams.recv_window_update(frame));
                    */
                }
                None => {
                    // TODO: Is this correct?
                    trace!("codec closed");
                    return Ok(Async::Ready(()));
                }
            }
        }
    }

    /*
    pub fn send_data(self,
                     id: StreamId,
                     data: B,
                     end_of_stream: bool)
        -> sink::Send<Self>
    {
        self.send(Frame::Data {
            id,
            data,
            end_of_stream,
        })
    }

    pub fn send_trailers(self,
                         id: StreamId,
                         headers: HeaderMap)
        -> sink::Send<Self>
    {
        self.send(Frame::Trailers {
            id,
            headers,
        })
    }
    */

    // ===== Private =====

    /// Returns `Ready` when the `Connection` is ready to receive a frame from
    /// the socket.
    fn poll_recv_ready(&mut self) -> Poll<(), ConnectionError> {
        // Pong, settings ack, and stream refusals are high priority frames to
        // send. If the write buffer is full, we stop reading any further frames
        // until these high priority writes can be committed to the buffer.

        try_ready!(self.ping_pong.send_pending_pong(&mut self.codec));
        try_ready!(self.settings.send_pending_ack(&mut self.codec));
        try_ready!(self.streams.send_pending_refusal(&mut self.codec));

        Ok(().into())
    }

    /// Returns `Ready` when the `Connection` is ready to accept a frame from
    /// the user
    ///
    /// This function is currently used by poll_complete, but at some point it
    /// will probably not be required.
    fn poll_send_ready(&mut self) -> Poll<(), ConnectionError> {
        // TODO: Is this function needed?
        try_ready!(self.poll_recv_ready());

        Ok(().into())
    }

    fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
        try_ready!(self.poll_send_ready());

        // Ensure all window updates have been sent.
        try_ready!(self.streams.poll_complete(&mut self.codec));
        try_ready!(self.codec.poll_complete());

        Ok(().into())
    }

    fn convert_poll_message(frame: frame::Headers) -> Result<Frame<P::Poll>, ConnectionError> {
        if frame.is_trailers() {
            Ok(Frame::Trailers {
                id: frame.stream_id(),
                headers: frame.into_fields()
            })
        } else {
            Ok(Frame::Headers {
                id: frame.stream_id(),
                end_of_stream: frame.is_end_stream(),
                headers: P::convert_poll_message(frame)?,
            })
        }
    }
}

impl<T, B> Connection<T, client::Peer, B>
    where T: AsyncRead + AsyncWrite,
          B: IntoBuf,
{
    /// Initialize a new HTTP/2.0 stream and send the message.
    pub fn send_request(&mut self, request: Request<()>, end_of_stream: bool)
        -> Result<StreamRef<client::Peer, B::Buf>, ConnectionError>
    {
        self.streams.send_request(request, end_of_stream)
    }
}

/*
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

    pub fn send_push_promise(self,
                             id: StreamId,
                             promised_id: StreamId)
        -> sink::Send<Self>
    {
        self.send(Frame::PushPromise {
            id,
            promised_id,
        })
    }
}
*/

/*
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
        // TODO: Ensure connection is not corrupt

        // Ensure that the connection is ready to accept a new frame
        if !try!(self.poll_ready()).is_ready() {
            return Ok(AsyncSink::NotReady(item));
        }

        let frame = match item {
            Frame::Headers { id, headers, end_of_stream } => {
                // This is a one-way conversion. By checking `poll_ready` first (above),
                // it's already been determined that the inner `Sink` can accept the item.
                // If the item is rejected, then there is a bug.
                let frame = P::convert_send_message(id, headers, end_of_stream);

                // Update the stream state
                self.streams.send_headers(&frame)?;

                frame::Frame::Headers(frame)
            }
            Frame::Data { id, data, end_of_stream } => {
                let frame = frame::Data::from_buf(
                    id, data.into_buf(), end_of_stream);

                self.streams.send_data(&frame)?;

                frame.into()
            }
            Frame::Reset { id, error } => frame::Reset::new(id, error).into(),
            Frame::Trailers { id, headers } => {
                let mut frame = frame::Headers::new(
                    id,
                    frame::Pseudo::default(),
                    headers);

                frame.set_end_stream();

                self.streams.send_headers(&frame)?;

                frame::Frame::Headers(frame)
            }
            /*
            Frame::PushPromise { id, promise } => {
                unimplemented!();
            }
            Frame::Error { id, error } => {
                unimplemented!();
            }
            */
            _ => unimplemented!(),
        };

        // Write the frame to the socket
        let res = self.codec.start_send(frame)?;

        // Ensure that the write was accepted. This is always true due to the
        // check at the top of the function
        assert!(res.is_ready());

        // Return success
        Ok(AsyncSink::Ready)
    }
}
*/
