use super::DecoderError;
use util::byte_str::{ByteStr, FromUtf8Error};

use http::{Method, StatusCode};
use http::header::{HeaderName, HeaderValue};
use bytes::Bytes;

/// HPack table entry
#[derive(Debug, Clone)]
pub enum Entry {
    Header {
        name: HeaderName,
        value: HeaderValue,
    },
    Authority(ByteStr),
    Method(Method),
    Scheme(ByteStr),
    Path(ByteStr),
    Status(StatusCode),
}

/// The name component of an Entry
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum Key<'a> {
    Header(&'a HeaderName),
    Authority,
    Method,
    Scheme,
    Path,
    Status,
}

pub fn len(name: &HeaderName, value: &HeaderValue) -> usize {
    let n: &str = name.as_ref();
    32 + n.len() + value.len()
}

impl Entry {
    pub fn new(name: Bytes, value: Bytes) -> Result<Entry, DecoderError> {
        if name[0] == b':' {
            match &name[1..] {
                b"authority" => {
                    let value = try!(ByteStr::from_utf8(value));
                    Ok(Entry::Authority(value))
                }
                b"method" => {
                    let method = try!(Method::from_bytes(&value));
                    Ok(Entry::Method(method))
                }
                b"scheme" => {
                    let value = try!(ByteStr::from_utf8(value));
                    Ok(Entry::Scheme(value))
                }
                b"path" => {
                    let value = try!(ByteStr::from_utf8(value));
                    Ok(Entry::Path(value))
                }
                b"status" => {
                    let status = try!(StatusCode::from_slice(&value));
                    Ok(Entry::Status(status))
                }
                _ => {
                    Err(DecoderError::InvalidPseudoheader)
                }
            }
        } else {
            let name = try!(HeaderName::from_bytes(&name));
            let value = try!(HeaderValue::try_from_slice(&value));

            Ok(Entry::Header { name: name, value: value })
        }
    }

    pub fn len(&self) -> usize {
        match *self {
            Entry::Header { ref name, ref value } => {
                len(name, value)
            }
            Entry::Authority(ref v) => {
                32 + 10 + v.len()
            }
            Entry::Method(ref v) => {
                32 + 7 + v.as_ref().len()
            }
            Entry::Scheme(ref v) => {
                32 + 7 + v.len()
            }
            Entry::Path(ref v) => {
                32 + 5 + v.len()
            }
            Entry::Status(ref v) => {
                32 + 7 + 3
            }
        }
    }

    pub fn key(&self) -> Key {
        match *self {
            Entry::Header { ref name, .. } => Key::Header(name),
            Entry::Authority(..) => Key::Authority,
            Entry::Method(..) => Key::Method,
            Entry::Scheme(..) => Key::Scheme,
            Entry::Path(..) => Key::Path,
            Entry::Status(..) => Key::Status,
        }
    }

    pub fn value_eq(&self, other: &Entry) -> bool {
        match *self {
            Entry::Header { ref value, .. } => {
                let a = value;
                match *other {
                    Entry::Header { ref value, .. } => a == value,
                    _ => false,
                }
            }
            Entry::Authority(ref a) => {
                match *other {
                    Entry::Authority(ref b) => a == b,
                    _ => false,
                }
            }
            Entry::Method(ref a) => {
                match *other {
                    Entry::Method(ref b) => a == b,
                    _ => false,
                }
            }
            Entry::Scheme(ref a) => {
                match *other {
                    Entry::Scheme(ref b) => a == b,
                    _ => false,
                }
            }
            Entry::Path(ref a) => {
                match *other {
                    Entry::Path(ref b) => a == b,
                    _ => false,
                }
            }
            Entry::Status(ref a) => {
                match *other {
                    Entry::Status(ref b) => a == b,
                    _ => false,
                }
            }
        }
    }

    pub fn skip_value_index(&self) -> bool {
        use http::header;

        match *self {
            Entry::Header { ref name, .. } => {
                match *name {
                    header::AGE |
                        header::AUTHORIZATION |
                        header::CONTENT_LENGTH |
                        header::ETAG |
                        header::IF_MODIFIED_SINCE |
                        header::IF_NONE_MATCH |
                        header::LOCATION |
                        header::COOKIE |
                        header::SET_COOKIE => true,
                    _ => false,
                }
            }
            Entry::Path(..) => true,
            _ => false,
        }
    }
}

impl<'a> Key<'a> {
    pub fn into_entry(self, value: Bytes) -> Result<Entry, DecoderError> {
        match self {
            Key::Header(name) => {
                Ok(Entry::Header {
                    name: name.clone(),
                    value: try!(HeaderValue::try_from_slice(&*value)),
                })
            }
            Key::Authority => {
                Ok(Entry::Authority(try!(ByteStr::from_utf8(value))))
            }
            Key::Method => {
                Ok(Entry::Scheme(try!(ByteStr::from_utf8(value))))
            }
            Key::Scheme => {
                Ok(Entry::Scheme(try!(ByteStr::from_utf8(value))))
            }
            Key::Path => {
                Ok(Entry::Path(try!(ByteStr::from_utf8(value))))
            }
            Key::Status => {
                match StatusCode::from_slice(&value) {
                    Ok(status) => Ok(Entry::Status(status)),
                    // TODO: better error handling
                    Err(_) => Err(DecoderError::InvalidStatusCode),
                }
            }
        }
    }
}
