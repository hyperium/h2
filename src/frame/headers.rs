use super::{StreamId, StreamDependency};
use hpack;
use frame::{self, Frame, Head, Kind, Error};
use HeaderMap;

use http::{uri, Method, StatusCode, Uri};
use http::header::{self, HeaderName, HeaderValue};

use bytes::{BytesMut, Bytes};
use byteorder::{BigEndian, ByteOrder};
use string::String;

use std::fmt;
use std::io::Cursor;

/// Header frame
///
/// This could be either a request or a response.
pub struct Headers {
    /// The ID of the stream with which this frame is associated.
    stream_id: StreamId,

    /// The stream dependency information, if any.
    stream_dep: Option<StreamDependency>,

    /// The decoded header fields
    fields: HeaderMap,

    /// Pseudo headers, these are broken out as they must be sent as part of the
    /// headers frame.
    pseudo: Pseudo,

    /// The associated flags
    flags: HeadersFlag,
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub struct HeadersFlag(u8);

#[derive(Debug)]
pub struct PushPromise {
    /// The ID of the stream with which this frame is associated.
    stream_id: StreamId,

    /// The ID of the stream being reserved by this PushPromise.
    promised_id: StreamId,

    /// The associated flags
    flags: PushPromiseFlag,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct PushPromiseFlag(u8);

#[derive(Debug)]
pub struct Continuation {
    /// Stream ID of continuation frame
    stream_id: StreamId,

    /// Argument to pass to the HPACK encoder to resume encoding
    hpack: hpack::EncodeState,

    /// remaining headers to encode
    headers: Iter,
}

#[derive(Debug, Default)]
pub struct Pseudo {
    // Request
    pub method: Option<Method>,
    pub scheme: Option<String<Bytes>>,
    pub authority: Option<String<Bytes>>,
    pub path: Option<String<Bytes>>,

    // Response
    pub status: Option<StatusCode>,
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
    /// Create a new HEADERS frame
    pub fn new(stream_id: StreamId, pseudo: Pseudo, fields: HeaderMap) -> Self {
        Headers {
            stream_id: stream_id,
            stream_dep: None,
            fields: fields,
            pseudo: pseudo,
            flags: HeadersFlag::default(),
        }
    }

    pub fn trailers(stream_id: StreamId, fields: HeaderMap) -> Self {
        let mut flags = HeadersFlag::default();
        flags.set_end_stream();

        Headers {
            stream_id,
            stream_dep: None,
            fields: fields,
            pseudo: Pseudo::default(),
            flags: flags,
        }
    }

    /// Loads the header frame but doesn't actually do HPACK decoding.
    ///
    /// HPACK decoding is done in the `load_hpack` step.
    pub fn load(head: Head, mut src: BytesMut)
        -> Result<(Self, BytesMut), Error>
    {
        let flags = HeadersFlag(head.flag());
        let mut pad = 0;

        trace!("loading headers; flags={:?}", flags);

        // Read the padding length
        if flags.is_padded() {
            // TODO: Ensure payload is sized correctly
            pad = src[0] as usize;

            // Drop the padding
            let _ = src.split_to(1);
        }

        // Read the stream dependency
        let stream_dep = if flags.is_priority() {
            let stream_dep = StreamDependency::load(&src[..5])?;

            if stream_dep.dependency_id() == head.stream_id() {
                return Err(Error::InvalidDependencyId);
            }

            // Drop the next 5 bytes
            let _ = src.split_to(5);

            Some(stream_dep)
        } else {
            None
        };

        if pad > 0 {
            if pad > src.len() {
                return Err(Error::TooMuchPadding);
            }

            let len = src.len() - pad;
            src.truncate(len);
        }

        let headers = Headers {
            stream_id: head.stream_id(),
            stream_dep: stream_dep,
            fields: HeaderMap::new(),
            pseudo: Pseudo::default(),
            flags: flags,
        };

        Ok((headers, src))
    }

    pub fn load_hpack(&mut self,
                      src: BytesMut,
                      decoder: &mut hpack::Decoder)
        -> Result<(), Error>
    {
        let mut reg = false;
        let mut malformed = false;

        macro_rules! set_pseudo {
            ($field:ident, $val:expr) => {{
                if reg {
                    trace!("load_hpack; header malformed -- pseudo not at head of block");
                    malformed = true;
                } else if self.pseudo.$field.is_some() {
                    trace!("load_hpack; header malformed -- repeated pseudo");
                    malformed = true;
                } else {
                    self.pseudo.$field = Some($val);
                }
            }}
        }

        let mut src = Cursor::new(src.freeze());

        // At this point, we're going to assume that the hpack encoded headers
        // contain the entire payload. Later, we need to check for stream
        // priority.
        //
        // TODO: Provide a way to abort decoding if an error is hit.
        let res = decoder.decode(&mut src, |header| {
            use hpack::Header::*;

            match header {
                Field { name, value } => {
                    // Connection level header fields are not supported and must
                    // result in a protocol error.

                    if name == header::CONNECTION {
                        trace!("load_hpack; connection level header");
                        malformed = true;
                    } else if name == header::TE && value != "trailers" {
                        trace!("load_hpack; TE header not set to trailers; val={:?}", value);
                        malformed = true;
                    } else {
                        reg = true;
                        self.fields.append(name, value);
                    }
                }
                Authority(v) => set_pseudo!(authority, v),
                Method(v) => set_pseudo!(method, v),
                Scheme(v) => set_pseudo!(scheme, v),
                Path(v) => set_pseudo!(path, v),
                Status(v) => set_pseudo!(status, v),
            }
        });

        if let Err(e) = res {
            trace!("hpack decoding error; err={:?}", e);
            return Err(e.into());
        }

        if malformed {
            trace!("malformed message");
            return Err(Error::MalformedMessage.into());
        }

        Ok(())
    }

    pub fn stream_id(&self) -> StreamId {
        self.stream_id
    }

    pub fn is_end_headers(&self) -> bool {
        self.flags.is_end_headers()
    }

    pub fn is_end_stream(&self) -> bool {
        self.flags.is_end_stream()
    }

    pub fn set_end_stream(&mut self) {
        self.flags.set_end_stream()
    }

    pub fn into_parts(self) -> (Pseudo, HeaderMap) {
        (self.pseudo, self.fields)
    }

    pub fn into_fields(self) -> HeaderMap {
        self.fields
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
        BigEndian::write_uint(&mut dst[pos..pos+3], len as u64, 3);

        ret
    }

    fn head(&self) -> Head {
        Head::new(Kind::Headers, self.flags.into(), self.stream_id)
    }
}

impl<T> From<Headers> for Frame<T> {
    fn from(src: Headers) -> Self {
        Frame::Headers(src)
    }
}

impl fmt::Debug for Headers {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Headers")
            .field("stream_id", &self.stream_id)
            .field("stream_dep", &self.stream_dep)
            .field("flags", &self.flags)
            // `fields` and `pseudo` purposefully not included
            .finish()
    }
}

// ===== impl PushPromise =====

impl PushPromise {
    pub fn load(head: Head, payload: &[u8])
        -> Result<Self, Error>
    {
        let flags = PushPromiseFlag(head.flag());

        // TODO: Handle padding

        let (promised_id, _) = StreamId::parse(&payload[..4]);

        Ok(PushPromise {
            stream_id: head.stream_id(),
            promised_id: promised_id,
            flags: flags,
        })
    }

    pub fn stream_id(&self) -> StreamId {
        self.stream_id
    }

    pub fn promised_id(&self) -> StreamId {
        self.promised_id
    }
}

impl<T> From<PushPromise> for Frame<T> {
    fn from(src: PushPromise) -> Self {
        Frame::PushPromise(src)
    }
}

// ===== impl Pseudo =====

impl Pseudo {
    pub fn request(method: Method, uri: Uri) -> Self {
        let parts = uri::Parts::from(uri);

        fn to_string(src: Bytes) -> String<Bytes> {
            unsafe { String::from_utf8_unchecked(src) }
        }

        let path = parts.path_and_query
            .map(|v| v.into())
            .unwrap_or_else(|| Bytes::from_static(b"/"));

        let mut pseudo = Pseudo {
            method: Some(method),
            scheme: None,
            authority: None,
            path: Some(to_string(path)),
            status: None,
        };

        // If the URI includes a scheme component, add it to the pseudo headers
        //
        // TODO: Scheme must be set...
        if let Some(scheme) = parts.scheme {
            pseudo.set_scheme(to_string(scheme.into()));
        }

        // If the URI includes an authority component, add it to the pseudo
        // headers
        if let Some(authority) = parts.authority {
            pseudo.set_authority(to_string(authority.into()));
        }

        pseudo
    }

    pub fn response(status: StatusCode) -> Self {
        Pseudo {
            method: None,
            scheme: None,
            authority: None,
            path: None,
            status: Some(status),
        }
    }

    pub fn set_scheme(&mut self, scheme: String<Bytes>) {
        self.scheme = Some(scheme);
    }

    pub fn set_authority(&mut self, authority: String<Bytes>) {
        self.authority = Some(authority);
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

    pub fn set_end_stream(&mut self) {
        self.0 |= END_STREAM
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

impl Default for HeadersFlag {
    /// Returns a `HeadersFlag` value with `END_HEADERS` set.
    fn default() -> Self {
        HeadersFlag(END_HEADERS)
    }
}

impl From<HeadersFlag> for u8 {
    fn from(src: HeadersFlag) -> u8 {
        src.0
    }
}

impl fmt::Debug for HeadersFlag {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("HeadersFlag")
            .field("end_stream", &self.is_end_stream())
            .field("end_headers", &self.is_end_headers())
            .field("padded", &self.is_padded())
            .field("priority", &self.is_priority())
            .finish()
    }
}
