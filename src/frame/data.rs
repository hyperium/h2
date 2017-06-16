use frame::{util, Head, Error, StreamId, Kind};
use bytes::{BufMut, Bytes};

#[derive(Debug)]
pub struct Data {
    stream_id: StreamId,
    data: Bytes,
    flags: DataFlag,
    pad_len: Option<u8>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct DataFlag(u8);

const END_STREAM: u8 = 0x1;
const PADDED: u8 = 0x8;
const ALL: u8 = END_STREAM | PADDED;

impl Data {
    pub fn load(head: Head, mut payload: Bytes) -> Result<Data, Error> {
        let flags = DataFlag::load(head.flag());

        let pad_len = if flags.is_padded() {
            let len = try!(util::strip_padding(&mut payload));
            Some(len)
        } else {
            None
        };

        Ok(Data {
            stream_id: head.stream_id(),
            data: payload,
            flags: flags,
            pad_len: pad_len,
        })
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn encode<T: BufMut>(&self, dst: &mut T) {
        self.head().encode(self.len(), dst);
        dst.put(&self.data);
    }

    pub fn head(&self) -> Head {
        Head::new(Kind::Data, self.flags.into(), self.stream_id)
    }

    pub fn into_payload(self) -> Bytes {
        self.data
    }
}


impl DataFlag {
    pub fn load(bits: u8) -> DataFlag {
        DataFlag(bits & ALL)
    }

    pub fn end_stream() -> DataFlag {
        DataFlag(END_STREAM)
    }

    pub fn padded() -> DataFlag {
        DataFlag(PADDED)
    }

    pub fn is_end_stream(&self) -> bool {
        self.0 & END_STREAM == END_STREAM
    }

    pub fn is_padded(&self) -> bool {
        self.0 & PADDED == PADDED
    }
}

impl From<DataFlag> for u8 {
    fn from(src: DataFlag) -> u8 {
        src.0
    }
}
