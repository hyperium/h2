use super::Error;

use bytes::{BufMut, BigEndian};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Head {
    kind: Kind,
    flag: u8,
    stream_id: StreamId,
}

#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Kind {
    Data = 0,
    Headers = 1,
    Priority = 2,
    Reset = 3,
    Settings = 4,
    PushPromise = 5,
    Ping = 6,
    GoAway = 7,
    WindowUpdate = 8,
    Continuation = 9,
    Unknown,
}

pub type StreamId = u32;

const STREAM_ID_MASK: StreamId = 0x80000000;

// ===== impl Head =====

impl Head {
    pub fn new(kind: Kind, flag: u8, stream_id: StreamId) -> Head {
        Head {
            kind: kind,
            flag: flag,
            stream_id: stream_id,
        }
    }

    /// Parse an HTTP/2.0 frame header
    pub fn parse(header: &[u8]) -> Head {
        Head {
            kind: Kind::new(header[3]),
            flag: header[4],
            stream_id: parse_stream_id(&header[5..]),
        }
    }

    pub fn stream_id(&self) -> u32 {
        self.stream_id
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn flag(&self) -> u8 {
        self.flag
    }

    pub fn encode_len(&self) -> usize {
        super::HEADER_LEN
    }

    pub fn encode<T: BufMut>(&self, payload_len: usize, dst: &mut T) -> Result<(), Error> {
        debug_assert_eq!(self.encode_len(), dst.remaining_mut());
        debug_assert!(self.stream_id & STREAM_ID_MASK == 0);

        dst.put_uint::<BigEndian>(payload_len as u64, 3);
        dst.put_u8(self.kind as u8);
        dst.put_u8(self.flag);
        dst.put_u32::<BigEndian>(self.stream_id);
        Ok(())
    }
}

/// Parse the next 4 octets in the given buffer, assuming they represent an
/// HTTP/2 stream ID.  This means that the most significant bit of the first
/// octet is ignored and the rest interpreted as a network-endian 31-bit
/// integer.
#[inline]
fn parse_stream_id(buf: &[u8]) -> StreamId {
    let unpacked = unpack_octets_4!(buf, 0, u32);
    // Now clear the most significant bit, as that is reserved and MUST be ignored when received.
    unpacked & !STREAM_ID_MASK
}

// ===== impl Kind =====

impl Kind {
    pub fn new(byte: u8) -> Kind {
        return match byte {
            0 => Kind::Data,
            1 => Kind::Headers,
            2 => Kind::Priority,
            3 => Kind::Reset,
            4 => Kind::Settings,
            5 => Kind::PushPromise,
            6 => Kind::Ping,
            7 => Kind::GoAway,
            8 => Kind::WindowUpdate,
            9 => Kind::Continuation,
            _ => Kind::Unknown,
        }
    }
}
