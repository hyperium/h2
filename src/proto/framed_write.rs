use {ConnectionError, Reason};
use frame::{Frame, Error};

use tokio_io::AsyncWrite;
use futures::*;
use bytes::{BytesMut, Buf, BufMut};

use std::io::{self, Cursor};

#[derive(Debug)]
pub struct FramedWrite<T> {
    inner: T,
    buf: Cursor<BytesMut>,
}

const DEFAULT_BUFFER_CAPACITY: usize = 8 * 1_024;
const MAX_BUFFER_CAPACITY: usize = 16 * 1_024;

impl<T: AsyncWrite> FramedWrite<T> {
    pub fn new(inner: T) -> FramedWrite<T> {
        let buf = BytesMut::with_capacity(DEFAULT_BUFFER_CAPACITY);

        FramedWrite {
            inner: inner,
            buf: Cursor::new(buf),
        }
    }

    fn write_buf(&mut self) -> &mut BytesMut {
        self.buf.get_mut()
    }
}

impl<T: AsyncWrite> Sink for FramedWrite<T> {
    type SinkItem = Frame;
    type SinkError = ConnectionError;

    fn start_send(&mut self, item: Frame) -> StartSend<Frame, ConnectionError> {
        let len = item.encode_len();

        if len > MAX_BUFFER_CAPACITY {
            // This case should never happen. Large frames should be chunked at
            // a higher level, so this is an internal error.
            return Err(ConnectionError::Proto(Reason::InternalError));
        }

        if self.write_buf().remaining_mut() <= len {
            // Try flushing the buffer
            try!(self.poll_complete());

            let rem = self.write_buf().remaining_mut();
            let additional = len - rem;

            if self.write_buf().capacity() + additional > MAX_BUFFER_CAPACITY {
                return Ok(AsyncSink::NotReady(item));
            }

            // Grow the buffer
            self.write_buf().reserve(additional);
        }

        // At this point, the buffer contains enough space
        item.encode(self.write_buf());

        Ok(AsyncSink::Ready)
    }

    fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
        while self.buf.has_remaining() {
            try_ready!(self.inner.write_buf(&mut self.buf));

            if !self.buf.has_remaining() {
                // Reset the buffer
                self.write_buf().clear();
                self.buf.set_position(0);
            }
        }

        // Try flushing the underlying IO
        try_nb!(self.inner.flush());

        return Ok(Async::Ready(()));
    }

    fn close(&mut self) -> Poll<(), ConnectionError> {
        try_ready!(self.poll_complete());
        self.inner.shutdown().map_err(Into::into)
    }
}
