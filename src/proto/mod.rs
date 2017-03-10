mod framed_read;
mod framed_write;

use {frame, ConnectionError};
use self::framed_read::FramedRead;
use self::framed_write::FramedWrite;

use tokio_io::{AsyncRead, AsyncWrite};
use tokio_io::codec::length_delimited;

use futures::*;

pub struct Connection<T> {
    inner: Inner<T>,
}

type Inner<T> =
    FramedWrite<
        FramedRead<
            length_delimited::FramedRead<T>>>;

impl<T: AsyncRead + AsyncWrite> Connection<T> {
    pub fn new(io: T) -> Connection<T> {
        // Delimit the frames
        let framed_read = length_delimited::Builder::new()
            .big_endian()
            .length_field_length(3)
            .length_adjustment(6)
            .num_skip(0) // Don't skip the header
            .new_read(io);

        // Map to `Frame` types
        let framed_read = FramedRead::new(framed_read);

        // Frame encoder
        let framed = FramedWrite::new(framed_read);

        Connection {
            inner: framed,
        }
    }
}

impl<T: AsyncRead + AsyncWrite> Stream for Connection<T> {
    type Item = frame::Frame;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Option<frame::Frame>, ConnectionError> {
        self.inner.poll()
    }
}

impl<T: AsyncRead + AsyncWrite> Sink for Connection<T> {
    type SinkItem = frame::Frame;
    type SinkError = ConnectionError;

    fn start_send(&mut self, item: frame::Frame) -> StartSend<frame::Frame, ConnectionError> {
        self.inner.start_send(item)
    }

    fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
        self.inner.poll_complete()
    }
}
