use codec::RecvError;
use frame::{self, Frame, Kind};
use frame::DEFAULT_SETTINGS_HEADER_TABLE_SIZE;
use frame::Reason::*;

use hpack;

use futures::*;

use bytes::BytesMut;

use tokio_io::AsyncRead;
use tokio_io::codec::length_delimited;

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
    // Decode the Continuation frame but ignore it...
    // Ignore(StreamId),
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

    fn decode_frame(&mut self, mut bytes: BytesMut) -> Result<Option<Frame>, RecvError> {
        use self::RecvError::*;

        trace!("decoding frame from {}B", bytes.len());

        // Parse the head
        let head = frame::Head::parse(&bytes);

        if self.partial.is_some() && head.kind() != Kind::Continuation {
            return Err(Connection(ProtocolError));
        }

        let kind = head.kind();

        trace!("    -> kind={:?}", kind);

        let frame = match kind {
            Kind::Settings => {
                let res = frame::Settings::load(head, &bytes[frame::HEADER_LEN..]);

                res.map_err(|_| Connection(ProtocolError))?.into()
            },
            Kind::Ping => {
                let res = frame::Ping::load(head, &bytes[frame::HEADER_LEN..]);

                res.map_err(|_| Connection(ProtocolError))?.into()
            },
            Kind::WindowUpdate => {
                let res = frame::WindowUpdate::load(head, &bytes[frame::HEADER_LEN..]);

                res.map_err(|_| Connection(ProtocolError))?.into()
            },
            Kind::Data => {
                let _ = bytes.split_to(frame::HEADER_LEN);
                let res = frame::Data::load(head, bytes.freeze());

                // TODO: Should this always be connection level? Probably not...
                res.map_err(|_| Connection(ProtocolError))?.into()
            },
            Kind::Headers => {
                // Drop the frame header
                // TODO: Change to drain: carllerche/bytes#130
                let _ = bytes.split_to(frame::HEADER_LEN);

                // Parse the header frame w/o parsing the payload
                let (mut headers, payload) = match frame::Headers::load(head, bytes) {
                    Ok(res) => res,
                    Err(frame::Error::InvalidDependencyId) => {
                        // A stream cannot depend on itself. An endpoint MUST
                        // treat this as a stream error (Section 5.4.2) of type
                        // `PROTOCOL_ERROR`.
                        return Err(Stream {
                            id: head.stream_id(),
                            reason: ProtocolError,
                        });
                    },
                    _ => return Err(Connection(ProtocolError)),
                };

                if headers.is_end_headers() {
                    // Load the HPACK encoded headers & return the frame
                    match headers.load_hpack(payload, &mut self.hpack) {
                        Ok(_) => {},
                        Err(frame::Error::MalformedMessage) => {
                            return Err(Stream {
                                id: head.stream_id(),
                                reason: ProtocolError,
                            });
                        },
                        Err(_) => return Err(Connection(ProtocolError)),
                    }

                    headers.into()
                } else {
                    // Defer loading the frame
                    self.partial = Some(Partial {
                        frame: Continuable::Headers(headers),
                        buf: payload,
                    });

                    return Ok(None);
                }
            },
            Kind::Reset => {
                let res = frame::Reset::load(head, &bytes[frame::HEADER_LEN..]);
                res.map_err(|_| Connection(ProtocolError))?.into()
            },
            Kind::GoAway => {
                let res = frame::GoAway::load(&bytes[frame::HEADER_LEN..]);
                res.map_err(|_| Connection(ProtocolError))?.into()
            },
            Kind::PushPromise => {
                let res = frame::PushPromise::load(head, &bytes[frame::HEADER_LEN..]);
                res.map_err(|_| Connection(ProtocolError))?.into()
            },
            Kind::Priority => {
                if head.stream_id() == 0 {
                    // Invalid stream identifier
                    return Err(Connection(ProtocolError));
                }

                match frame::Priority::load(head, &bytes[frame::HEADER_LEN..]) {
                    Ok(frame) => frame.into(),
                    Err(frame::Error::InvalidDependencyId) => {
                        // A stream cannot depend on itself. An endpoint MUST
                        // treat this as a stream error (Section 5.4.2) of type
                        // `PROTOCOL_ERROR`.
                        return Err(Stream {
                            id: head.stream_id(),
                            reason: ProtocolError,
                        });
                    },
                    Err(_) => return Err(Connection(ProtocolError)),
                }
            },
            Kind::Continuation => {
                // TODO: Un-hack this
                let end_of_headers = (head.flag() & 0x4) == 0x4;

                let mut partial = match self.partial.take() {
                    Some(partial) => partial,
                    None => return Err(Connection(ProtocolError)),
                };

                // Extend the buf
                partial.buf.extend_from_slice(&bytes[frame::HEADER_LEN..]);

                if !end_of_headers {
                    self.partial = Some(partial);
                    return Ok(None);
                }

                match partial.frame {
                    Continuable::Headers(mut frame) => {
                        // The stream identifiers must match
                        if frame.stream_id() != head.stream_id() {
                            return Err(Connection(ProtocolError));
                        }

                        match frame.load_hpack(partial.buf, &mut self.hpack) {
                            Ok(_) => {},
                            Err(frame::Error::MalformedMessage) => {
                                return Err(Stream {
                                    id: head.stream_id(),
                                    reason: ProtocolError,
                                });
                            },
                            Err(_) => return Err(Connection(ProtocolError)),
                        }

                        frame.into()
                    },
                }
            },
            Kind::Unknown => {
                // Unknown frames are ignored
                return Ok(None);
            },
        };

        Ok(Some(frame))
    }

    pub fn get_ref(&self) -> &T {
        self.inner.get_ref()
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.inner.get_mut()
    }

    /// Returns the current max frame size setting
    #[cfg(feature = "unstable")]
    #[inline]
    pub fn max_frame_size(&self) -> usize {
        self.inner.max_frame_length()
    }

    /// Updates the max frame size setting.
    #[cfg(feature = "unstable")]
    #[inline]
    pub fn set_max_frame_size(&mut self, val: usize) {
        self.inner.set_max_frame_length(val)
    }
}

impl<T> Stream for FramedRead<T>
where
    T: AsyncRead,
{
    type Item = Frame;
    type Error = RecvError;

    fn poll(&mut self) -> Poll<Option<Frame>, Self::Error> {
        loop {
            trace!("poll");
            let bytes = match try_ready!(self.inner.poll()) {
                Some(bytes) => bytes,
                None => return Ok(Async::Ready(None)),
            };

            trace!("poll; bytes={}B", bytes.len());
            if let Some(frame) = self.decode_frame(bytes)? {
                debug!("received; frame={:?}", frame);
                return Ok(Async::Ready(Some(frame)));
            }
        }
    }
}
