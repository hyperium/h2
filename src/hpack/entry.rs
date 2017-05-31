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
pub enum Key<'a> {
    Header(&'a HeaderName),
    Authority,
    Method,
    Scheme,
    Path,
    Status,
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
                let n: &str = name.as_ref();
                32 + n.len() + value.len()
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
