use {hpack, ConnectionError};
use frame::{self, Frame, Kind};
use frame::DEFAULT_SETTINGS_HEADER_TABLE_SIZE;
use proto::*;
use error::Reason::*;

use futures::*;

use bytes::{Bytes, BytesMut};

use tokio_io::AsyncRead;
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
struct Partial {
    /// Empty frame
    frame: Continuable,

    /// Partial header payload
    buf: BytesMut,
}

#[derive(Debug)]
enum Continuable {
    Headers(frame::Headers),
    // PushPromise(frame::PushPromise),
}

impl<T> FramedRead<T> {
    pub fn new(inner: length_delimited::FramedRead<T>) -> FramedRead<T> {
        FramedRead {
            inner: inner,
            hpack: hpack::Decoder::new(DEFAULT_SETTINGS_HEADER_TABLE_SIZE),
            partial: None,
        }
    }

    fn decode_frame(&mut self, mut bytes: BytesMut) -> Result<Option<Frame>, ConnectionError> {
        trace!("decoding frame from {}B", bytes.len());

        // Parse the head
        let head = frame::Head::parse(&bytes);

        if self.partial.is_some() && head.kind() != Kind::Continuation {
            return Err(ProtocolError.into());
        }

        let kind = head.kind();

        trace!("    -> kind={:?}", kind);

        let frame = match kind {
            Kind::Settings => {
                frame::Settings::load(head, &bytes[frame::HEADER_LEN..])?.into()
            }
            Kind::Ping => {
                frame::Ping::load(head, &bytes[frame::HEADER_LEN..])?.into()
            }
            Kind::WindowUpdate => {
                frame::WindowUpdate::load(head, &bytes[frame::HEADER_LEN..])?.into()
            }
            Kind::Data => {
                let _ = bytes.split_to(frame::HEADER_LEN);
                frame::Data::load(head, bytes.freeze())?.into()
            }
            Kind::Headers => {
                // Drop the frame header
                // TODO: Change to drain: carllerche/bytes#130
                let _ = bytes.split_to(frame::HEADER_LEN);

                // Parse the header frame w/o parsing the payload
                let (mut headers, payload) = frame::Headers::load(head, bytes)?;

                if headers.is_end_headers() {
                    // Load the HPACK encoded headers & return the frame
                    headers.load_hpack(payload, &mut self.hpack)?;
                    headers.into()
                } else {
                    // Defer loading the frame
                    self.partial = Some(Partial {
                        frame: Continuable::Headers(headers),
                        buf: payload,
                    });

                    return Ok(None);
                }
            }
            Kind::Reset => {
                frame::Reset::load(head, &bytes[frame::HEADER_LEN..])?.into()
            }
            Kind::GoAway => {
                frame::GoAway::load(&bytes[frame::HEADER_LEN..])?.into()
            }
            Kind::PushPromise => {
                frame::PushPromise::load(head, &bytes[frame::HEADER_LEN..])?.into()
            }
            Kind::Priority => {
                frame::Priority::load(head, &bytes[frame::HEADER_LEN..])?.into()
            }
            Kind::Continuation => {
                // TODO: Un-hack this
                let end_of_headers = (head.flag() & 0x4) == 0x4;

                let mut partial = match self.partial.take() {
                    Some(partial) => partial,
                    None => return Err(ProtocolError.into()),
                };

                // Extend the buf
                partial.buf.extend_from_slice(&bytes[frame::HEADER_LEN..]);

                if !end_of_headers {
                    self.partial = Some(partial);
                    return Ok(None);
                }

                match partial.frame {
                    Continuable::Headers(mut frame) => {
                        frame.load_hpack(partial.buf, &mut self.hpack)?;
                        frame.into()
                    }
                }
            }
            Kind::Unknown => {
                // Unknown frames are ignored
                return Ok(None);
            }
        };

        Ok(Some(frame))
    }
}

impl<T> futures::Stream for FramedRead<T>
    where T: AsyncRead,
{
    type Item = Frame;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Option<Frame>, ConnectionError> {
        loop {
            trace!("poll");
            let bytes = match try_ready!(self.inner.poll()) {
                Some(bytes) => bytes,
                None => return Ok(Async::Ready(None)),
            };

            trace!("poll; bytes={}B", bytes.len());
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
        self.inner.get_mut().start_send(item)
    }

    fn poll_complete(&mut self) -> Poll<(), T::SinkError> {
        self.inner.get_mut().poll_complete()
    }
}

impl<T: AsyncWrite, B: Buf> FramedRead<FramedWrite<T, B>> {
    pub fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
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
