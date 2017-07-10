use bytes::{Buf, BufMut, IntoBuf};
use frame::{Frame, Head, Kind, Error, StreamId};

const ACK_FLAG: u8 = 0x1;

pub type Payload = [u8; 8];

#[derive(Debug)]
pub struct Ping {
    ack: bool,
    payload: Payload,
}

impl Ping {
    pub fn ping(payload: Payload) -> Ping {
        Ping { ack: false, payload }
    }

    pub fn pong(payload: Payload) -> Ping {
        Ping { ack: true, payload }
    }

    pub fn is_ack(&self) -> bool {
        self.ack
    }

    pub fn into_payload(self) -> Payload {
        self.payload
    }

    /// Builds a `Ping` frame from a raw frame.
    pub fn load(head: Head, bytes: &[u8]) -> Result<Ping, Error> {
        debug_assert_eq!(head.kind(), ::frame::Kind::Ping);

        // PING frames are not associated with any individual stream. If a PING
        // frame is received with a stream identifier field value other than
        // 0x0, the recipient MUST respond with a connection error
        // (Section 5.4.1) of type PROTOCOL_ERROR.
        if !head.stream_id().is_zero() {
            return Err(Error::InvalidStreamId);
        }

        // In addition to the frame header, PING frames MUST contain 8 octets of opaque
        // data in the payload.
        if bytes.len() != 8 {
            return Err(Error::BadFrameSize);
        }

        let mut payload = [0; 8];
        bytes.into_buf().copy_to_slice(&mut payload);

        // The PING frame defines the following flags:
        //
        // ACK (0x1): When set, bit 0 indicates that this PING frame is a PING
        //    response. An endpoint MUST set this flag in PING responses. An
        //    endpoint MUST NOT respond to PING frames containing this flag.
        let ack = head.flag() & ACK_FLAG != 0;

        Ok(Ping { ack, payload })
    }

    pub fn encode<B: BufMut>(&self, dst: &mut B) {
        let sz = self.payload.len();
        trace!("encoding PING; ack={} len={}", self.ack, sz);

        let flags = if self.ack { ACK_FLAG } else { 0 };
        let head = Head::new(Kind::Ping, flags, StreamId::zero());

        head.encode(sz, dst);
        dst.put_slice(&self.payload);
    }
}

impl<T> From<Ping> for Frame<T> {
    fn from(src: Ping) -> Frame<T> {
        Frame::Ping(src)
    }
}
