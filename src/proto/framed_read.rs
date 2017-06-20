use {hpack, ConnectionError};
use frame::{self, Frame, Kind};
use frame::DEFAULT_SETTINGS_HEADER_TABLE_SIZE;

use tokio_io::AsyncWrite;

use futures::*;
use bytes::{Bytes, BytesMut, Buf};

use std::io::{self, Write, Cursor};

pub struct FramedRead<T> {
    inner: T,

    // hpack decoder state
    hpack: hpack::Decoder,

    partial: Option<Partial>,
}

/// Partially loaded headers frame
enum Partial {
    Headers(frame::Headers),
    PushPromise(frame::PushPromise),
}

impl<T> FramedRead<T>
    where T: Stream<Item = BytesMut, Error = io::Error>,
          T: AsyncWrite,
{
    pub fn new(inner: T) -> FramedRead<T> {
        FramedRead {
            inner: inner,
            hpack: hpack::Decoder::new(DEFAULT_SETTINGS_HEADER_TABLE_SIZE),
            partial: None,
        }
    }
}

impl<T> FramedRead<T> {
    fn decode_frame(&mut self, mut bytes: Bytes) -> Result<Option<Frame>, ConnectionError> {
        // Parse the head
        let head = frame::Head::parse(&bytes);

        if self.partial.is_some() && head.kind() != Kind::Continuation {
            unimplemented!();
        }

        let frame = match head.kind() {
            Kind::Data => unimplemented!(),
            Kind::Headers => {
                let mut buf = Cursor::new(bytes);
                buf.set_position(frame::HEADER_LEN as u64);

                // TODO: Change to drain: carllerche/bytes#130
                let frame = try!(frame::Headers::load(head, &mut buf, &mut self.hpack));
                frame.into()
            }
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
            Kind::Unknown => return Ok(None),
        };

        Ok(Some(frame))
    }
}

impl<T> Stream for FramedRead<T>
    where T: Stream<Item = BytesMut, Error = io::Error>,
{
    type Item = Frame;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Option<Frame>, ConnectionError> {
        loop {
            let mut bytes = match try_ready!(self.inner.poll()) {
                Some(bytes) => bytes.freeze(),
                None => return Ok(Async::Ready(None)),
            };

            if let Some(frame) = try!(self.decode_frame(bytes)) {
                return Ok(Async::Ready(Some(frame)));
            }
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
