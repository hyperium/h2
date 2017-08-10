use error::Reason;
use frame::{self, Head, Error, Kind, StreamId};

use bytes::{BufMut, BigEndian};

#[derive(Debug)]
pub struct GoAway {
    last_stream_id: StreamId,
    error_code: u32,
}

impl GoAway {
    pub fn reason(&self) -> Reason {
        self.error_code.into()
    }

    pub fn load(payload: &[u8]) -> Result<GoAway, Error> {
        if payload.len() < 8 {
            // Invalid payload len
            // TODO: Handle error
            unimplemented!();
        }

        let (last_stream_id, _) = StreamId::parse(&payload[..4]);
        let error_code = unpack_octets_4!(payload, 4, u32);

        Ok(GoAway {
            last_stream_id: last_stream_id,
            error_code: error_code,
        })
    }

    pub fn encode<B: BufMut>(&self, dst: &mut B) {
        trace!("encoding GO_AWAY; code={}", self.error_code);
        let head = Head::new(Kind::GoAway, 0, StreamId::zero());
        head.encode(8, dst);
        dst.put_u32::<BigEndian>(self.last_stream_id.into());
        dst.put_u32::<BigEndian>(self.error_code);
    }
}

impl<B> From<GoAway> for frame::Frame<B> {
    fn from(src: GoAway) -> Self {
        frame::Frame::GoAway(src)
    }
}
