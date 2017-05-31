use bytes::Bytes;

use std::{ops, str};
use std::str::Utf8Error;

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ByteStr {
    bytes: Bytes,
}

pub struct FromUtf8Error {
    err: Utf8Error,
    val: Bytes,
}

impl ByteStr {
    #[inline]
    pub fn from_static(val: &'static str) -> ByteStr {
        ByteStr { bytes: Bytes::from_static(val.as_bytes()) }
    }

    #[inline]
    pub unsafe fn from_utf8_unchecked(bytes: Bytes) -> ByteStr {
        ByteStr { bytes: bytes }
    }

    pub fn from_utf8(bytes: Bytes) -> Result<ByteStr, FromUtf8Error> {
        if let Err(e) = str::from_utf8(&bytes[..]) {
            return Err(FromUtf8Error {
                err: e,
                val: bytes,
            });
        }

        Ok(ByteStr { bytes: bytes })
    }
}

impl ops::Deref for ByteStr {
    type Target = str;

    #[inline]
    fn deref(&self) -> &str {
        let b: &[u8] = self.bytes.as_ref();
        unsafe { str::from_utf8_unchecked(b) }
    }
}

impl From<String> for ByteStr {
    #[inline]
    fn from(src: String) -> ByteStr {
        ByteStr { bytes: Bytes::from(src) }
    }
}

impl<'a> From<&'a str> for ByteStr {
    #[inline]
    fn from(src: &'a str) -> ByteStr {
        ByteStr { bytes: Bytes::from(src) }
    }
}
