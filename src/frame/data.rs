use frame::{util, Frame, Head, Error, StreamId, Kind};
use bytes::{BufMut, Bytes, Buf};

#[derive(Debug)]
pub struct Data<T = Bytes> {
    stream_id: StreamId,
    data: T,
    flags: DataFlag,
    pad_len: Option<u8>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct DataFlag(u8);

const END_STREAM: u8 = 0x1;
const PADDED: u8 = 0x8;
const ALL: u8 = END_STREAM | PADDED;

impl Data<Bytes> {
    pub fn load(head: Head, mut payload: Bytes) -> Result<Self, Error> {
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
}

impl<T> Data<T> {
    pub fn stream_id(&self) -> StreamId {
        self.stream_id
    }

    pub fn is_end_stream(&self) -> bool {
        self.flags.is_end_stream()
    }

    pub fn set_end_stream(&mut self) {
        self.flags.set_end_stream();
    }

    pub fn unset_end_stream(&mut self) {
        self.flags.unset_end_stream();
    }

    pub fn head(&self) -> Head {
        Head::new(Kind::Data, self.flags.into(), self.stream_id)
    }

    pub fn payload(&self) -> &T {
        &self.data
    }

    pub fn payload_mut(&mut self) -> &mut T {
        &mut self.data
    }

    pub fn into_payload(self) -> T {
        self.data
    }

    pub fn map<F, U>(self, f: F) -> Data<U>
        where F: FnOnce(T) -> U,
    {
        Data {
            stream_id: self.stream_id,
            data: f(self.data),
            flags: self.flags,
            pad_len: self.pad_len,
        }
    }
}

impl<T: Buf> Data<T> {
    pub fn from_buf(stream_id: StreamId, data: T, eos: bool) -> Self {
        // TODO ensure that data.remaining() < MAX_FRAME_SIZE
        let mut flags = DataFlag::default();
        if eos {
            flags.set_end_stream();
        }
        Data {
            stream_id,
            data,
            flags,
            pad_len: None,
        }
    }

    pub fn encode_chunk<U: BufMut>(&mut self, dst: &mut U) {
        let len = self.data.remaining() as usize;
        if len > dst.remaining_mut() {
            unimplemented!();
        }

        self.head().encode(len, dst);
        dst.put(&mut self.data);
    }
}

impl<T> From<Data<T>> for Frame<T> {
    fn from(src: Data<T>) -> Self {
        Frame::Data(src)
    }
}

// ===== impl DataFlag =====

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

    pub fn set_end_stream(&mut self) {
        self.0 |= END_STREAM
    }

    pub fn unset_end_stream(&mut self) {
        self.0 &= !END_STREAM
    }

    pub fn is_padded(&self) -> bool {
        self.0 & PADDED == PADDED
    }
}

impl Default for DataFlag {
    /// Returns a `HeadersFlag` value with `END_HEADERS` set.
    fn default() -> Self {
        DataFlag(0)
    }
}

impl From<DataFlag> for u8 {
    fn from(src: DataFlag) -> u8 {
        src.0
    }
}
