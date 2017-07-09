use StreamId;
use byteorder::{ByteOrder, NetworkEndian};
use bytes::{BufMut};
use frame::{self, Head, Kind, Error};

const INCREMENT_MASK: u32 = 1 << 31;

type Increment = u32;

#[derive(Debug)]
pub struct WindowUpdate {
    stream_id: StreamId,
    increment: Increment,
}

impl WindowUpdate {
    pub fn stream_id(&self) -> StreamId {
        self.stream_id
    }

    pub fn increment(&self) -> Increment {
        self.increment
    }

        /// Builds a `Ping` frame from a raw frame.
    pub fn load(head: Head, bytes: &[u8]) -> Result<WindowUpdate, Error> {
        debug_assert_eq!(head.kind(), ::frame::Kind::WindowUpdate);
        Ok(WindowUpdate {
            stream_id: head.stream_id(),
            // Clear the most significant bit, as that is reserved and MUST be ignored when
            // received.
            increment: NetworkEndian::read_u32(bytes) & !INCREMENT_MASK,
        })
    }

    pub fn encode<B: BufMut>(&self, dst: &mut B) {
        trace!("encoding WINDOW_UPDATE; id={:?}", self.stream_id);
        let head = Head::new(Kind::Ping, 0, self.stream_id);
        head.encode(4, dst);
        dst.put_u32::<NetworkEndian>(self.increment);
    }
}

impl From<WindowUpdate> for frame::Frame {
    fn from(src: WindowUpdate) -> frame::Frame {
        frame::Frame::WindowUpdate(src)
    }
}
