use {hpack, ConnectionError};
use frame::{self, Frame, Kind};
use frame::DEFAULT_SETTINGS_HEADER_TABLE_SIZE;
use proto::ReadySink;

use futures::*;

use bytes::Bytes;

use tokio_io::{AsyncRead};
use tokio_io::codec::length_delimited;

use std::io::{self, Cursor};

#[derive(Debug)]
pub struct FramedRead<T> {
    inner: length_delimited::FramedRead<T>,

    // hpack decoder state
    hpack: hpack::Decoder,

    partial: Option<Partial>,
}

/// Partially loaded headers frame
#[derive(Debug)]
enum Partial {
    Headers(frame::Headers),
    // PushPromise(frame::PushPromise),
}

impl<T> FramedRead<T>
    where T: AsyncRead,
          T: Sink<SinkItem = Frame, SinkError = ConnectionError>,
{
    pub fn new(inner: length_delimited::FramedRead<T>) -> FramedRead<T> {
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
            Kind::Data => {
                let _ = bytes.split_to(frame::HEADER_LEN);
                let frame = try!(frame::Data::load(head, bytes));
                frame.into()
            }
            Kind::Headers => {
                let mut buf = Cursor::new(bytes);
                buf.set_position(frame::HEADER_LEN as u64);

                // TODO: Change to drain: carllerche/bytes#130
                let frame = try!(frame::Headers::load(head, &mut buf, &mut self.hpack));

                if !frame.is_end_headers() {
                    // Wait for continuation frames
                    self.partial = Some(Partial::Headers(frame));
                    return Ok(None);
                }

                frame.into()
            }
            Kind::Priority => unimplemented!(),
            Kind::Reset => {
                let frame = try!(frame::Reset::load(head, &bytes[frame::HEADER_LEN..]));
                debug!("decoded; frame={:?}", frame);
                // TODO: implement
                return Ok(None);
            }
            Kind::Settings => {
                let frame = try!(frame::Settings::load(head, &bytes[frame::HEADER_LEN..]));
                frame.into()
            }
            Kind::PushPromise => {
                debug!("received PUSH_PROMISE");
                // TODO: implement
                return Ok(None);
            }
            Kind::Ping => {
                try!(frame::Ping::load(head, &bytes[frame::HEADER_LEN..])).into()
            }
            Kind::GoAway => {
                let frame = try!(frame::GoAway::load(&bytes[frame::HEADER_LEN..]));
                debug!("decoded; frame={:?}", frame);
                unimplemented!();
            }
            Kind::WindowUpdate => {
                let frame = try!(frame::WindowUpdate::load(head, &bytes[frame::HEADER_LEN..]));
                frame.into()
            }
            Kind::Continuation => {
                unimplemented!();
            }
            Kind::Unknown => return Ok(None),
        };

        Ok(Some(frame))
    }
}

impl<T> Stream for FramedRead<T>
    where T: AsyncRead,
{
    type Item = Frame;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Option<Frame>, ConnectionError> {
        loop {
            let bytes = match try_ready!(self.inner.poll()) {
                Some(bytes) => bytes.freeze(),
                None => return Ok(Async::Ready(None)),
            };

            if let Some(frame) = try!(self.decode_frame(bytes)) {
                debug!("poll; frame={:?}", frame);
                return Ok(Async::Ready(Some(frame)));
            }
        }
    }
}

impl<T: Sink> Sink for FramedRead<T> {
    type SinkItem = T::SinkItem;
    type SinkError = T::SinkError;

    fn start_send(&mut self, item: T::SinkItem) -> StartSend<T::SinkItem, T::SinkError> {
        self.inner.get_mut().start_send(item)
    }

    fn poll_complete(&mut self) -> Poll<(), T::SinkError> {
        self.inner.get_mut().poll_complete()
    }
}

impl<T: ReadySink> ReadySink for FramedRead<T> {
    fn poll_ready(&mut self) -> Poll<(), Self::SinkError> {
        self.inner.get_mut().poll_ready()
    }
}

impl<T: io::Write> io::Write for FramedRead<T> {
    fn write(&mut self, src: &[u8]) -> io::Result<usize> {
        self.inner.get_mut().write(src)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.get_mut().flush()
    }
}
