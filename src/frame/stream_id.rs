use byteorder::{BigEndian, ByteOrder};
use std::u32;

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct StreamId(u32);

const STREAM_ID_MASK: u32 = 1 << 31;

impl StreamId {
    /// Parse the stream ID
    #[inline]
    pub fn parse(buf: &[u8]) -> StreamId {
        let unpacked = BigEndian::read_u32(buf);
        // Now clear the most significant bit, as that is reserved and MUST be
        // ignored when received.
        StreamId(unpacked & !STREAM_ID_MASK)
    }

    pub fn is_client_initiated(&self) -> bool {
        let id = self.0;
        id != 0 && id % 2 == 1
    }

    pub fn is_server_initiated(&self) -> bool {
        let id = self.0;
        id != 0 && id % 2 == 0
    }

    #[inline]
    pub fn zero() -> StreamId {
        StreamId(0)
    }

    #[inline]
    pub fn max() -> StreamId {
        StreamId(u32::MAX >> 1)
    }

    pub fn is_zero(&self) -> bool {
        self.0 == 0
    }

    pub fn increment(&mut self) {
        self.0 += 2;
    }
}

impl From<u32> for StreamId {
    fn from(src: u32) -> Self {
        assert_eq!(src & STREAM_ID_MASK, 0, "invalid stream ID -- MSB is set");
        StreamId(src)
    }
}

impl From<StreamId> for u32 {
    fn from(src: StreamId) -> Self {
        src.0
    }
}

impl PartialEq<u32> for StreamId {
    fn eq(&self, other: &u32) -> bool {
        self.0 == *other
    }
}
