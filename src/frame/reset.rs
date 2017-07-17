use error::Reason;
use frame::{self, Head, Error, Kind, StreamId};

use bytes::{BufMut, BigEndian};

#[derive(Debug)]
pub struct Reset {
    stream_id: StreamId,
    error_code: u32,
}

impl Reset {
    pub fn new(stream_id: StreamId, error: Reason) -> Reset {
        Reset {
            stream_id,
            error_code: error.into(),
        }
    }
    pub fn load(head: Head, payload: &[u8]) -> Result<Reset, Error> {
        if payload.len() != 4 {
            return Err(Error::InvalidPayloadLength);
        }

        let error_code = unpack_octets_4!(payload, 0, u32);

        Ok(Reset {
            stream_id: head.stream_id(),
            error_code: error_code,
        })
    }

    pub fn encode<B: BufMut>(&self, dst: &mut B) {
        trace!("encoding RESET; id={:?} code={}", self.stream_id, self.error_code);
        let head = Head::new(Kind::Reset, 0, self.stream_id);
        head.encode(4, dst);
        dst.put_u32::<BigEndian>(self.error_code);
    }

    pub fn stream_id(&self) -> StreamId {
        self.stream_id
    }

    pub fn reason(&self) -> Reason {
        self.error_code.into()
    }
}

impl<B> From<Reset> for frame::Frame<B> {
    fn from(src: Reset) -> Self {
        frame::Frame::Reset(src)
    }
}
