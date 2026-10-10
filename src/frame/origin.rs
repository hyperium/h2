use bytes::BufMut;

use crate::frame::{self, Head, Kind, StreamId};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Origin {
    origins: Vec<String>,
}

impl Origin {
    /// Create a new ORIGIN frame.
    ///
    /// Entries that are not ASCII or too long to encode are skipped.
    pub fn new<I, S>(origins: I) -> Origin
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Origin {
            origins: Self::valid_origins(origins).collect(),
        }
    }

    /// Returns the origins that can be encoded in an ORIGIN frame.
    pub(crate) fn valid_origins<I, S>(origins: I) -> impl Iterator<Item = String>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        origins
            .into_iter()
            .map(Into::into)
            .filter(|origin| origin.is_ascii() && origin.len() <= u16::MAX as usize)
    }

    pub fn into_origins(self) -> Vec<String> {
        self.origins
    }

    /// Builds an `Origin` frame from a raw frame.
    pub fn load(head: Head, mut payload: &[u8]) -> Origin {
        debug_assert_eq!(head.kind(), crate::frame::Kind::Origin);

        let mut origins = Vec::new();

        // Entries that are not ASCII are skipped, and so is a truncated
        // entry at the end of the payload.
        while payload.len() >= 2 {
            let len = u16::from_be_bytes([payload[0], payload[1]]) as usize;
            if payload.len() < 2 + len {
                break;
            }
            let entry = &payload[2..2 + len];
            payload = &payload[2 + len..];
            if let Ok(origin) = std::str::from_utf8(entry) {
                if origin.is_ascii() {
                    origins.push(origin.to_owned());
                }
            }
        }

        Origin { origins }
    }

    pub fn encode<B: BufMut>(&self, dst: &mut B) {
        let len = self.origins.iter().map(|origin| 2 + origin.len()).sum();
        tracing::trace!("encoding ORIGIN; len={}", len);
        let head = Head::new(Kind::Origin, 0, StreamId::zero());
        head.encode(len, dst);
        for origin in &self.origins {
            dst.put_u16(origin.len() as u16);
            dst.put_slice(origin.as_bytes());
        }
    }
}

impl<B> From<Origin> for frame::Frame<B> {
    fn from(src: Origin) -> Self {
        frame::Frame::Origin(src)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_the_entries() {
        let mut buf = Vec::new();
        Origin::new([
            "https://a.example",
            "https://b.example:8443",
            "https://ä.example",
        ])
        .encode(&mut buf);

        let mut want = vec![0, 0, 2 + 17 + 2 + 22, 0x0c, 0, 0, 0, 0, 0, 0, 17];
        want.extend_from_slice(b"https://a.example");
        want.extend_from_slice(&[0, 22]);
        want.extend_from_slice(b"https://b.example:8443");
        assert_eq!(buf, want);
    }

    #[test]
    fn loads_the_entries() {
        let mut buf = Vec::new();
        Origin::new(["https://a.example", "https://b.example:8443"]).encode(&mut buf);
        // A truncated entry at the end is dropped.
        buf.extend_from_slice(&[0, 9, b'a']);

        let head = Head::parse(&buf[..frame::HEADER_LEN]);
        let origin = Origin::load(head, &buf[frame::HEADER_LEN..]);
        assert_eq!(
            origin.into_origins(),
            ["https://a.example", "https://b.example:8443"]
        );
    }

    #[test]
    fn empty_frame() {
        let mut buf = Vec::new();
        Origin::new(Vec::<String>::new()).encode(&mut buf);
        assert_eq!(buf, [0, 0, 0, 0x0c, 0, 0, 0, 0, 0]);
    }
}
