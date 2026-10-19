//! Extensions specific to the HTTP/2 protocol.
//!
//! [RFC 9113 Section 5.5][ext] permits an extension to define new frame types,
//! new settings, or new error codes, and obliges an endpoint to ignore what it
//! does not understand, which is what makes an extension point usable
//! "without prior arrangement or negotiation". h2 implements no extension. What
//! it offers is the means to carry one, and this module is only part of that:
//!
//! - **`SETTINGS` parameters.** Identifiers come from the IANA ["HTTP/2
//!   Settings" registry][reg]; h2 carries the number without defining it.
//!   [`client::Builder::setting`] and
//!   [`server::Builder::setting`] send one by identifier;
//!   [`SendRequest::peer_settings`] and [`server::Connection::peer_settings`]
//!   read back what the peer sent. h2 applies no rule to an identifier it does
//!   not define, and rejects no value of one.
//! - **Error codes.** [`Reason`] is a `u32` either way, so a code this crate
//!   does not name is carried verbatim.
//! - **Frame types.** A frame whose type h2 does not know is discarded, as 5.5
//!   requires. Sending one is not supported.
//!
//! [`Protocol`] below is a different thing: not an HTTP/2 extension point, but
//! the `:protocol` pseudo-header of [RFC 8441][8441], the extension that lets
//! a CONNECT name another protocol to carry on its stream. h2 checks the name
//! is UTF-8 and does not otherwise interpret it, so a protocol h2 has never
//! heard of works without a change here.
//!
//! [ext]: https://datatracker.ietf.org/doc/html/rfc9113#section-5.5
//! [reg]: https://www.iana.org/assignments/http2-parameters/http2-parameters.xhtml
//! [8441]: https://datatracker.ietf.org/doc/html/rfc8441#section-4
//! [`client::Builder::setting`]: crate::client::Builder::setting
//! [`server::Builder::setting`]: crate::server::Builder::setting
//! [`SendRequest::peer_settings`]: crate::client::SendRequest::peer_settings
//! [`server::Connection::peer_settings`]: crate::server::Connection::peer_settings
//! [`Reason`]: crate::Reason

use crate::hpack::BytesStr;

use bytes::Bytes;
use std::fmt;

/// Represents the `:protocol` pseudo-header used by
/// the [Extended CONNECT Protocol].
///
/// [Extended CONNECT Protocol]: https://datatracker.ietf.org/doc/html/rfc8441#section-4
#[derive(Clone, Eq, PartialEq)]
pub struct Protocol {
    value: BytesStr,
}

impl Protocol {
    /// Converts a static string to a protocol name.
    pub const fn from_static(value: &'static str) -> Self {
        Self {
            value: BytesStr::from_static(value),
        }
    }

    /// Returns a str representation of the header.
    pub fn as_str(&self) -> &str {
        self.value.as_str()
    }

    pub(crate) fn try_from(bytes: Bytes) -> Result<Self, std::str::Utf8Error> {
        Ok(Self {
            value: BytesStr::try_from(bytes)?,
        })
    }
}

impl<'a> From<&'a str> for Protocol {
    fn from(value: &'a str) -> Self {
        Self {
            value: BytesStr::from(value),
        }
    }
}

impl AsRef<[u8]> for Protocol {
    fn as_ref(&self) -> &[u8] {
        self.value.as_ref()
    }
}

impl fmt::Debug for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.value.fmt(f)
    }
}
