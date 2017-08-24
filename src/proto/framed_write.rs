use {hpack, ConnectionError};
use error::User::*;
use frame::{self, Frame, FrameSize};

use futures::*;
use tokio_io::{AsyncRead, AsyncWrite};
use bytes::{BytesMut, Buf, BufMut};

use std::io::{self, Cursor};

#[derive(Debug)]
pub struct FramedWrite<T, B> {
    /// Upstream `AsyncWrite`
    inner: T,

    /// HPACK encoder
    hpack: hpack::Encoder,

    /// Write buffer
    ///
    /// TODO: Should this be a ring buffer?
    buf: Cursor<BytesMut>,

    /// Next frame to encode
    next: Option<Next<B>>,

    /// Last data frame
    last_data_frame: Option<frame::Data<B>>,

    /// Max frame size, this is specified by the peer
    max_frame_size: FrameSize,
}

#[derive(Debug)]
enum Next<B> {
    Data(frame::Data<B>),
    Continuation(frame::Continuation),
}

/// Initialze the connection with this amount of write buffer.
const DEFAULT_BUFFER_CAPACITY: usize = 4 * 1_024;

/// Min buffer required to attempt to write a frame
const MIN_BUFFER_CAPACITY: usize = frame::HEADER_LEN + CHAIN_THRESHOLD;

/// Chain payloads bigger than this. The remote will never advertise a max frame
/// size less than this (well, the spec says the max frame size can't be less
/// than 16kb, so not even close).
const CHAIN_THRESHOLD: usize = 256;

// TODO: Make generic
impl<T, B> FramedWrite<T, B>
    where T: AsyncWrite,
          B: Buf,
{
    pub fn new(inner: T) -> FramedWrite<T, B> {
        FramedWrite {
            inner: inner,
            hpack: hpack::Encoder::default(),
            buf: Cursor::new(BytesMut::with_capacity(DEFAULT_BUFFER_CAPACITY)),
            next: None,
            last_data_frame: None,
            max_frame_size: frame::DEFAULT_MAX_FRAME_SIZE,
        }
    }

    pub fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        if !self.has_capacity() {
            // Try flushing
            try!(self.poll_complete());

            if !self.has_capacity() {
                return Ok(Async::NotReady);
            }
        }

        Ok(Async::Ready(()))
    }

    fn has_capacity(&self) -> bool {
        self.next.is_none() && self.buf.get_ref().remaining_mut() >= MIN_BUFFER_CAPACITY
    }

    fn is_empty(&self) -> bool {
        match self.next {
            Some(Next::Data(ref frame)) => !frame.payload().has_remaining(),
            _ => !self.buf.has_remaining(),
        }
    }
}

impl<T, B> FramedWrite<T, B> {
    pub fn max_frame_size(&self) -> usize {
        self.max_frame_size as usize
    }

    pub fn apply_remote_settings(&mut self, settings: &frame::Settings) {
        if let Some(val) = settings.max_frame_size() {
            self.max_frame_size = val;
        }
    }

    pub fn take_last_data_frame(&mut self) -> Option<frame::Data<B>> {
        self.last_data_frame.take()
    }
}

impl<T, B> Sink for FramedWrite<T, B>
    where T: AsyncWrite,
          B: Buf,
{
    type SinkItem = Frame<B>;
    type SinkError = ConnectionError;

    fn start_send(&mut self, item: Self::SinkItem)
        -> StartSend<Self::SinkItem, ConnectionError>
    {
        if !try!(self.poll_ready()).is_ready() {
            return Ok(AsyncSink::NotReady(item));
        }

        debug!("send; frame={:?}", item);

        match item {
            Frame::Data(mut v) => {
                // Ensure that the payload is not greater than the max frame.
                let len = v.payload().remaining();

                if len > self.max_frame_size() {
                    return Err(PayloadTooBig.into());
                }

                if len >= CHAIN_THRESHOLD {
                    let head = v.head();

                    // Encode the frame head to the buffer
                    head.encode(len, self.buf.get_mut());

                    // Save the data frame
                    self.next = Some(Next::Data(v));
                } else {
                    v.encode_chunk(self.buf.get_mut());

                    // The chunk has been fully encoded, so there is no need to
                    // keep it around
                    assert_eq!(v.payload().remaining(), 0, "chunk not fully encoded");

                    // Save off the last frame...
                    self.last_data_frame = Some(v);
                }
            }
            Frame::Headers(v) => {
                if let Some(continuation) = v.encode(&mut self.hpack, self.buf.get_mut()) {
                    self.next = Some(Next::Continuation(continuation));
                }
            }
            Frame::PushPromise(v) => {
                debug!("unimplemented PUSH_PROMISE write; frame={:?}", v);
                unimplemented!();
            }
            Frame::Settings(v) => {
                v.encode(self.buf.get_mut());
                trace!("encoded settings; rem={:?}", self.buf.remaining());
            }
            Frame::GoAway(v) => {
                v.encode(self.buf.get_mut());
                trace!("encoded go_away; rem={:?}", self.buf.remaining());
            }
            Frame::Ping(v) => {
                v.encode(self.buf.get_mut());
                trace!("encoded ping; rem={:?}", self.buf.remaining());
            }
            Frame::WindowUpdate(v) => {
                v.encode(self.buf.get_mut());
                trace!("encoded window_update; rem={:?}", self.buf.remaining());
            }

            Frame::Priority(_) => {
                /*
                v.encode(self.buf.get_mut());
                trace!("encoded priority; rem={:?}", self.buf.remaining());
                */
                unimplemented!();
            }
            Frame::Reset(v) => {
                v.encode(self.buf.get_mut());
                trace!("encoded reset; rem={:?}", self.buf.remaining());
            }
        }

        Ok(AsyncSink::Ready)
    }

    fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
        trace!("poll_complete");

        while !self.is_empty() {
            match self.next {
                Some(Next::Data(ref mut frame)) => {
                    let mut buf = Buf::by_ref(&mut self.buf).chain(frame.payload_mut());
                    try_ready!(self.inner.write_buf(&mut buf));
                }
                _ => {
                    try_ready!(self.inner.write_buf(&mut self.buf));
                }
            }
        }

        // The data frame has been written, so unset it
        match self.next.take() {
            Some(Next::Data(frame)) => {
                self.last_data_frame = Some(frame);
            }
            Some(Next::Continuation(_)) => {
                unimplemented!();
            }
            None => {}
        }

        trace!("flushing buffer");
        // Flush the upstream
        try_nb!(self.inner.flush());

        // Clear internal buffer
        self.buf.set_position(0);
        self.buf.get_mut().clear();

        Ok(Async::Ready(()))
    }

    fn close(&mut self) -> Poll<(), ConnectionError> {
        try_ready!(self.poll_complete());
        self.inner.shutdown().map_err(Into::into)
    }
}

impl<T: Stream, B> Stream for FramedWrite<T, B> {
    type Item = T::Item;
    type Error = T::Error;

    fn poll(&mut self) -> Poll<Option<T::Item>, T::Error> {
        self.inner.poll()
    }
}

impl<T: io::Read, B> io::Read for FramedWrite<T, B> {
    fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        self.inner.read(dst)
    }
}

impl<T: AsyncRead, B> AsyncRead for FramedWrite<T, B> {
    fn read_buf<B2: BufMut>(&mut self, buf: &mut B2) -> Poll<usize, io::Error>
        where Self: Sized,
    {
        self.inner.read_buf(buf)
    }

    unsafe fn prepare_uninitialized_buffer(&self, buf: &mut [u8]) -> bool {
        self.inner.prepare_uninitialized_buffer(buf)
    }
}
