use {hpack, ConnectionError, FrameSize};
use frame::{self, Frame};
use proto::{ApplySettings, ReadySink};

use futures::*;
use tokio_io::{AsyncRead, AsyncWrite};
use bytes::{BytesMut, Buf, BufMut};

use std::cmp;
use std::io::{self, Cursor};

#[derive(Debug)]
pub struct FramedWrite<T, B> {
    /// Upstream `AsyncWrite`
    inner: T,

    /// HPACK encoder
    hpack: hpack::Encoder,

    /// Write buffer
    buf: Cursor<BytesMut>,

    /// Next frame to encode
    next: Option<Next<B>>,

    /// Max frame size, this is specified by the peer
    max_frame_size: FrameSize,
}

#[derive(Debug)]
enum Next<B> {
    Data {
        /// Length of the current frame being written
        frame_len: usize,

        /// Data frame to encode
        data: frame::Data<B>,
    },
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
            max_frame_size: frame::DEFAULT_MAX_FRAME_SIZE,
        }
    }

    fn has_capacity(&self) -> bool {
        self.next.is_none() && self.buf.get_ref().remaining_mut() >= MIN_BUFFER_CAPACITY
    }

    fn is_empty(&self) -> bool {
        self.next.is_none() && !self.buf.has_remaining()
    }

    fn frame_len(&self, data: &frame::Data<B>) -> usize {
        cmp::min(self.max_frame_size as usize, data.payload().remaining())
    }
}

impl<T, B> ApplySettings for FramedWrite<T, B> {
    fn apply_local_settings(&mut self, _set: &frame::SettingSet) -> Result<(), ConnectionError> {
        Ok(())
    }

    fn apply_remote_settings(&mut self, _set: &frame::SettingSet) -> Result<(), ConnectionError> {
        Ok(())
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

        match item {
            Frame::Data(mut v) => {
                if v.payload().remaining() >= CHAIN_THRESHOLD {
                    let head = v.head();
                    let len = self.frame_len(&v);

                    // Encode the frame head to the buffer
                    head.encode(len, self.buf.get_mut());

                    // Save the data frame
                    self.next = Some(Next::Data {
                        frame_len: len,
                        data: v,
                    });
                } else {
                    v.encode_chunk(self.buf.get_mut());

                    // The chunk has been fully encoded, so there is no need to
                    // keep it around
                    assert_eq!(v.payload().remaining(), 0, "chunk not fully encoded");
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
            Frame::Ping(v) => {
                v.encode(self.buf.get_mut());
                trace!("encoded ping; rem={:?}", self.buf.remaining());
            }
            Frame::WindowUpdate(v) => {
                v.encode(self.buf.get_mut());
                trace!("encoded window_update; rem={:?}", self.buf.remaining());
            }
            Frame::Reset(v) => {
                v.encode(self.buf.get_mut());
                trace!("encoded reset; rem={:?}", self.buf.remaining());
            }
        }

        Ok(AsyncSink::Ready)
    }

    fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
        trace!("FramedWrite::poll_complete");

        // TODO: implement
        match self.next {
            Some(Next::Data { .. }) => unimplemented!(),
            _ => {}
        }

        // As long as there is data to write, try to write it!
        while !self.is_empty() {
            try_ready!(self.inner.write_buf(&mut self.buf));
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

impl<T, B> ReadySink for FramedWrite<T, B>
    where T: AsyncWrite,
          B: Buf,
{
    fn poll_ready(&mut self) -> Poll<(), Self::SinkError> {
        if !self.has_capacity() {
            // Try flushing
            try!(self.poll_complete());

            if !self.has_capacity() {
                return Ok(Async::NotReady);
            }
        }

        Ok(Async::Ready(()))
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
