use http::header::{HeaderMap, HeaderName, HeaderValue};
use bytes::BytesMut;

pub struct Encoder {
    table: HeaderMap<()>,

    // The remote sent a max size update, we must shrink the table on next call
    // to encode.
    max_size_update: Option<usize>,

    // Current max table size
    max_size: usize,
}

pub enum EncoderError {
}

impl Encoder {
    pub fn encode<'a, I>(&mut self, headers: I, dst: &mut BytesMut) -> Result<(), EncoderError>
        where I: IntoIterator<Item=(&'a HeaderName, &'a HeaderValue)>,
    {
        unimplemented!();
    }
}
