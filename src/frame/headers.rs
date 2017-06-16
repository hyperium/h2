use super::StreamId;
use util::byte_str::ByteStr;

use http::{Method, StatusCode};
use http::header::{self, HeaderMap, HeaderValue};

/// Header frame
///
/// This could be either a request or a response.
#[derive(Debug)]
pub struct Headers {
    /// The ID of the stream with which this frame is associated.
    stream_id: StreamId,

    /// The stream dependency information, if any.
    stream_dep: Option<StreamDependency>,

    /// The decoded headers
    headers: HeaderMap<HeaderValue>,

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

#[derive(Debug)]
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

    /// Headers
    headers: header::IntoIter<HeaderValue>,
}

const END_STREAM: u8 = 0x1;
const END_HEADERS: u8 = 0x4;
const PADDED: u8 = 0x8;
const PRIORITY: u8 = 0x20;
const ALL: u8 = END_STREAM
              | END_HEADERS
              | PADDED
              | PRIORITY;

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
