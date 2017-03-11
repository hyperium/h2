use frame::{Frame, Head, Error};
use bytes::{Bytes, BytesMut, BufMut};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unknown {
    head: Head,
    payload: Bytes,
}

impl Unknown {
    pub fn new(head: Head, payload: Bytes) -> Unknown {
        Unknown {
            head: head,
            payload: payload,
        }
    }

    pub fn encode_len(&self) -> usize {
        self.head.encode_len() + self.payload.len()
    }

    pub fn encode(&self, dst: &mut BytesMut) -> Result<(), Error> {
        try!(self.head.encode(self.payload.len(), dst));
        dst.put(&self.payload);
        Ok(())
    }
}

impl From<Unknown> for Frame {
    fn from(src: Unknown) -> Frame {
        Frame::Unknown(src)
    }
}
