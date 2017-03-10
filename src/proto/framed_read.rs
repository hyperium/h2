use ConnectionError;
use frame::Frame;

use tokio_io::AsyncWrite;

use futures::*;
use bytes::{BytesMut, Buf};

use std::io::{self, Write};

pub struct FramedRead<T> {
    inner: T,
}

impl<T> FramedRead<T>
    where T: Stream<Item = BytesMut, Error = io::Error>,
          T: AsyncWrite,
{
    pub fn new(inner: T) -> FramedRead<T> {
        FramedRead { inner: inner }
    }
}

impl<T> Stream for FramedRead<T>
    where T: Stream<Item = BytesMut, Error = io::Error>,
{
    type Item = Frame;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Option<Frame>, ConnectionError> {
        match try_ready!(self.inner.poll()) {
            Some(bytes) => {
                Frame::load(bytes.freeze())
                    .map(|frame| Async::Ready(Some(frame)))
                    .map_err(ConnectionError::from)
            }
            None => Ok(Async::Ready(None)),
        }
    }
}

impl<T: Sink> Sink for FramedRead<T> {
    type SinkItem = T::SinkItem;
    type SinkError = T::SinkError;

    fn start_send(&mut self, item: T::SinkItem) -> StartSend<T::SinkItem, T::SinkError> {
        self.inner.start_send(item)
    }

    fn poll_complete(&mut self) -> Poll<(), T::SinkError> {
        self.inner.poll_complete()
    }
}

impl<T: io::Write> io::Write for FramedRead<T> {
    fn write(&mut self, src: &[u8]) -> io::Result<usize> {
        self.inner.write(src)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<T: AsyncWrite> AsyncWrite for FramedRead<T> {
    fn shutdown(&mut self) -> Poll<(), io::Error> {
        self.inner.shutdown()
    }

    fn write_buf<B: Buf>(&mut self, buf: &mut B) -> Poll<usize, io::Error> {
        self.inner.write_buf(buf)
    }
}
