use super::Error;

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

// ===== impl Head =====

impl Head {
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
}

/// Parse the next 4 octets in the given buffer, assuming they represent an
/// HTTP/2 stream ID.  This means that the most significant bit of the first
/// octet is ignored and the rest interpreted as a network-endian 31-bit
/// integer.
#[inline]
fn parse_stream_id(buf: &[u8]) -> StreamId {
    let unpacked = unpack_octets_4!(buf, 0, u32);
    // Now clear the most significant bit, as that is reserved and MUST be ignored when received.
    unpacked & !0x80000000
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
