use super::DecoderError;

use tower::http::{HeaderName, Method, StatusCode, Str};
use bytes::Bytes;

/// HPack table entry
#[derive(Debug, Clone)]
pub enum Entry {
    Header {
        name: HeaderName,
        value: Str,
    },
    Authority(Str),
    Method(Method),
    Scheme(Str),
    Path(Str),
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
                    value: try!(Str::from_utf8(value)),
                })
            }
            Key::Authority => {
                Ok(Entry::Authority(try!(Str::from_utf8(value))))
            }
            Key::Method => {
                Ok(Entry::Scheme(try!(Str::from_utf8(value))))
            }
            Key::Scheme => {
                Ok(Entry::Scheme(try!(Str::from_utf8(value))))
            }
            Key::Path => {
                Ok(Entry::Path(try!(Str::from_utf8(value))))
            }
            Key::Status => {
                match StatusCode::parse(&value) {
                    Some(status) => Ok(Entry::Status(status)),
                    None => Err(DecoderError::InvalidStatusCode),
                }
            }
        }
    }
}
