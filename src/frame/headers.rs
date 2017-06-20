use super::StreamId;
use hpack;
use error::Reason;
use frame::{self, Frame, Head, Kind, Error};
use util::byte_str::ByteStr;

use http::{Method, StatusCode};
use http::header::{self, HeaderMap, HeaderName, HeaderValue};

use bytes::{BytesMut, Bytes};
use byteorder::{BigEndian, ByteOrder};

use std::io::Cursor;

/// Header frame
///
/// This could be either a request or a response.
#[derive(Debug)]
pub struct Headers {
    /// The ID of the stream with which this frame is associated.
    stream_id: StreamId,

    /// The stream dependency information, if any.
    stream_dep: Option<StreamDependency>,

    /// The decoded header fields
    fields: HeaderMap<HeaderValue>,

    /// Pseudo headers, these are broken out as they must be sent as part of the
    /// headers frame.
    pseudo: Pseudo,

    /// The associated flags
    flags: HeadersFlag,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
pub struct HeadersFlag(u8);

#[derive(Debug)]
pub struct PushPromise {
    /// The ID of the stream with which this frame is associated.
    stream_id: StreamId,

    /// The ID of the stream being reserved by this PushPromise.
    promised_id: StreamId,

    /// The associated flags
    flags: HeadersFlag,
}

#[derive(Debug)]
pub struct Continuation {
    /// Stream ID of continuation frame
    stream_id: StreamId,

    /// Argument to pass to the HPACK encoder to resume encoding
    hpack: hpack::EncodeState,

    /// remaining headers to encode
    headers: Iter,
}

#[derive(Debug)]
pub struct StreamDependency {
    /// The ID of the stream dependency target
    stream_id: StreamId,

    /// The weight for the stream. The value exposed (and set) here is always in
    /// the range [0, 255], instead of [1, 256] (as defined in section 5.3.2.)
    /// so that the value fits into a `u8`.
    weight: u8,

    /// True if the stream dependency is exclusive.
    is_exclusive: bool,
}

#[derive(Debug, Default)]
pub struct Pseudo {
    // Request
    method: Option<Method>,
    scheme: Option<ByteStr>,
    authority: Option<ByteStr>,
    path: Option<ByteStr>,

    // Response
    status: Option<StatusCode>,
}

#[derive(Debug)]
pub struct Iter {
    /// Pseudo headers
    pseudo: Option<Pseudo>,

    /// Header fields
    fields: header::IntoIter<HeaderValue>,
}

const END_STREAM: u8 = 0x1;
const END_HEADERS: u8 = 0x4;
const PADDED: u8 = 0x8;
const PRIORITY: u8 = 0x20;
const ALL: u8 = END_STREAM
              | END_HEADERS
              | PADDED
              | PRIORITY;

// ===== impl Headers =====

impl Headers {
    pub fn load(head: Head, src: &mut Cursor<Bytes>, decoder: &mut hpack::Decoder)
        -> Result<Self, Error>
    {
        let flags = HeadersFlag(head.flag());

        assert!(!flags.is_priority(), "unimplemented stream priority");

        let mut pseudo = Pseudo::default();
        let mut fields = HeaderMap::new();
        let mut err = false;

        macro_rules! set_pseudo {
            ($field:ident, $val:expr) => {{
                if pseudo.$field.is_some() {
                    err = true;
                } else {
                    pseudo.$field = Some($val);
                }
            }}
        }

        // At this point, we're going to assume that the hpack encoded headers
        // contain the entire payload. Later, we need to check for stream
        // priority.
        //
        // TODO: Provide a way to abort decoding if an error is hit.
        try!(decoder.decode(src, |header| {
            use hpack::Header::*;

            match header {
                Field { name, value } => {
                    fields.append(name, value);
                }
                Authority(v) => set_pseudo!(authority, v),
                Method(v) => set_pseudo!(method, v),
                Scheme(v) => set_pseudo!(scheme, v),
                Path(v) => set_pseudo!(path, v),
                Status(v) => set_pseudo!(status, v),
            }
        }));

        if err {
            return Err(hpack::DecoderError::RepeatedPseudo.into());
        }

        Ok(Headers {
            stream_id: head.stream_id(),
            stream_dep: None,
            fields: fields,
            pseudo: pseudo,
            flags: flags,
        })
    }

    pub fn encode(self, encoder: &mut hpack::Encoder, dst: &mut BytesMut)
        -> Option<Continuation>
    {
        let head = self.head();
        let pos = dst.len();

        // At this point, we don't know how big the h2 frame will be.
        // So, we write the head with length 0, then write the body, and
        // finally write the length once we know the size.
        head.encode(0, dst);

        // Encode the frame
        let mut headers = Iter {
            pseudo: Some(self.pseudo),
            fields: self.fields.into_iter(),
        };

        let ret = match encoder.encode(None, &mut headers, dst) {
            hpack::Encode::Full => None,
            hpack::Encode::Partial(state) => {
                Some(Continuation {
                    stream_id: self.stream_id,
                    hpack: state,
                    headers: headers,
                })
            }
        };

        // Compute the frame length
        let len = (dst.len() - pos) - frame::HEADER_LEN;

        // Write the frame length
        BigEndian::write_u32(&mut dst[pos..pos+3], len as u32);

        ret
    }

    fn head(&self) -> Head {
        Head::new(Kind::Data, self.flags.into(), self.stream_id)
    }
}

impl From<Headers> for Frame {
    fn from(src: Headers) -> Frame {
        Frame::Headers(src)
    }
}

// ===== impl Iter =====

impl Iterator for Iter {
    type Item = hpack::Header<Option<HeaderName>>;

    fn next(&mut self) -> Option<Self::Item> {
        use hpack::Header::*;

        if let Some(ref mut pseudo) = self.pseudo {
            if let Some(method) = pseudo.method.take() {
                return Some(Method(method));
            }

            if let Some(scheme) = pseudo.scheme.take() {
                return Some(Scheme(scheme));
            }

            if let Some(authority) = pseudo.authority.take() {
                return Some(Authority(authority));
            }

            if let Some(path) = pseudo.path.take() {
                return Some(Path(path));
            }

            if let Some(status) = pseudo.status.take() {
                return Some(Status(status));
            }
        }

        self.pseudo = None;

        self.fields.next()
            .map(|(name, value)| {
                Field { name: name, value: value}
            })
    }
}

// ===== impl HeadersFlag =====

impl HeadersFlag {
    pub fn empty() -> HeadersFlag {
        HeadersFlag(0)
    }

    pub fn load(bits: u8) -> HeadersFlag {
        HeadersFlag(bits & ALL)
    }

    pub fn is_end_stream(&self) -> bool {
        self.0 & END_STREAM == END_STREAM
    }

    pub fn is_end_headers(&self) -> bool {
        self.0 & END_HEADERS == END_HEADERS
    }

    pub fn is_padded(&self) -> bool {
        self.0 & PADDED == PADDED
    }

    pub fn is_priority(&self) -> bool {
        self.0 & PRIORITY == PRIORITY
    }
}

impl From<HeadersFlag> for u8 {
    fn from(src: HeadersFlag) -> u8 {
        src.0
    }
}
