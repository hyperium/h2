use {ConnectionError, Reason};
use frame::{self, Data, Frame, Error, Headers, PushPromise, Settings};
use hpack;

use futures::*;
use tokio_io::AsyncWrite;
use bytes::{Bytes, BytesMut, Buf, BufMut};
use http::header::{self, HeaderValue};

use std::cmp;
use std::io::{self, Cursor};

#[derive(Debug)]
pub struct FramedWrite<T> {
    /// Upstream `AsyncWrite`
    inner: T,

    /// HPACK encoder
    hpack: hpack::Encoder,

    /// Write buffer
    buf: BytesMut,

    /// Position in the frame
    pos: usize,

    /// Next frame to encode
    next: Option<Next>,

    /// Max frame size, this is specified by the peer
    max_frame_size: usize,
}

#[derive(Debug)]
enum Next {
    Data {
        /// Length of the current frame being written
        frame_len: usize,

        /// Data frame to encode
        data: frame::Data
    },
    Continuation {
        /// Stream ID of continuation frame
        stream_id: frame::StreamId,

        /// Argument to pass to the HPACK encoder to resume encoding
        resume: hpack::EncodeState,

        /// remaining headers to encode
        rem: header::IntoIter<HeaderValue>,
    },
}

/// Initialze the connection with this amount of write buffer.
const DEFAULT_BUFFER_CAPACITY: usize = 4 * 1_024;

/// Min buffer required to attempt to write a frame
const MIN_BUFFER_CAPACITY: usize = frame::HEADER_LEN + CHAIN_THRESHOLD;

/// Chain payloads bigger than this. The remote will never advertise a max frame
/// size less than this (well, the spec says the max frame size can't be less
/// than 16kb, so not even close).
const CHAIN_THRESHOLD: usize = 256;

impl<T: AsyncWrite> FramedWrite<T> {
    pub fn new(inner: T) -> FramedWrite<T> {
        FramedWrite {
            inner: inner,
            hpack: hpack::Encoder::default(),
            buf: BytesMut::with_capacity(DEFAULT_BUFFER_CAPACITY),
            pos: 0,
            next: None,
            max_frame_size: frame::DEFAULT_MAX_FRAME_SIZE,
        }
    }

    fn has_capacity(&self) -> bool {
        self.next.is_none() && self.buf.remaining_mut() >= MIN_BUFFER_CAPACITY
    }

    fn frame_len(&self, data: &Data) -> usize {
        cmp::min(self.max_frame_size, data.len())
    }
}

impl<T: AsyncWrite> Sink for FramedWrite<T> {
    type SinkItem = Frame;
    type SinkError = ConnectionError;

    fn start_send(&mut self, item: Frame) -> StartSend<Frame, ConnectionError> {
        if self.has_capacity() {
            // Try flushing
            try!(self.poll_complete());

            if self.has_capacity() {
                return Ok(AsyncSink::NotReady(item));
            }
        }

        match item {
            Frame::Data(v) => {
                if v.len() >= CHAIN_THRESHOLD {
                    let head = v.head();
                    let len = self.frame_len(&v);

                    // Encode the frame head to the buffer
                    head.encode(len, &mut self.buf);

                    // Save the data frame
                    self.next = Some(Next::Data {
                        frame_len: len,
                        data: v,
                    });
                } else {
                    v.encode(&mut self.buf);
                }
            }
            Frame::Headers(v) => {
                unimplemented!();
            }
            Frame::PushPromise(v) => {
                unimplemented!();
            }
            Frame::Settings(v) => {
                v.encode(&mut self.buf);
            }
        }

        Ok(AsyncSink::Ready)
    }

    fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
        unimplemented!();
    }

    fn close(&mut self) -> Poll<(), ConnectionError> {
        try_ready!(self.poll_complete());
        self.inner.shutdown().map_err(Into::into)
    }
}

impl<T: Stream> Stream for FramedWrite<T> {
    type Item = T::Item;
    type Error = T::Error;

    fn poll(&mut self) -> Poll<Option<T::Item>, T::Error> {
        self.inner.poll()
    }
}
