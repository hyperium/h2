use ConnectionError;
use frame::{self, Frame, Kind};

use tokio_io::AsyncWrite;

use futures::*;
use bytes::{BytesMut, Buf};

use std::io::{self, Write};

pub struct FramedRead<T> {
    inner: T,

    // hpack decoder state
    // hpack: hpack::Decoder,

}

impl<T> FramedRead<T>
    where T: Stream<Item = BytesMut, Error = io::Error>,
          T: AsyncWrite,
{
    pub fn new(inner: T) -> FramedRead<T> {
        FramedRead {
            inner: inner,
        }
    }
}

impl<T> Stream for FramedRead<T>
    where T: Stream<Item = BytesMut, Error = io::Error>,
{
    type Item = Frame;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Option<Frame>, ConnectionError> {
        let mut bytes = match try_ready!(self.inner.poll()) {
            Some(bytes) => bytes,
            None => return Ok(Async::Ready(None)),
        };

        // Parse the head
        let head = frame::Head::parse(&bytes);

        let frame = match head.kind() {
            Kind::Data => unimplemented!(),
            Kind::Headers => unimplemented!(),
            Kind::Priority => unimplemented!(),
            Kind::Reset => unimplemented!(),
            Kind::Settings => {
                let frame = try!(frame::Settings::load(head, &bytes[frame::HEADER_LEN..]));
                frame.into()
            }
            Kind::PushPromise => unimplemented!(),
            Kind::Ping => unimplemented!(),
            Kind::GoAway => unimplemented!(),
            Kind::WindowUpdate => unimplemented!(),
            Kind::Continuation => unimplemented!(),
            Kind::Unknown => {
                let _ = bytes.split_to(frame::HEADER_LEN);
                frame::Unknown::new(head, bytes.freeze()).into()
            }
            _ => unimplemented!(),
        };

        Ok(Async::Ready(Some(frame)))
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
