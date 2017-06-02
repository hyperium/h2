use super::entry;
use super::table::Table;

use http::header::{HeaderName, HeaderValue};
use bytes::BytesMut;

pub struct Encoder {
    table: Table,

    // The remote sent a max size update, we must shrink the table on next call
    // to encode. This is in bytes
    max_size_update: Option<usize>,
}

pub enum EncoderError {
}

impl Encoder {
    pub fn new() -> Encoder {
        Encoder {
            table: Table::with_capacity(0),
            max_size_update: None,
        }
    }

    pub fn encode<'a, I>(&mut self, headers: I, dst: &mut BytesMut) -> Result<(), EncoderError>
        where I: IntoIterator<Item=Entry>,
    {
        if let Some(max_size_update) = self.max_size_update.take() {
            // Write size update frame
            unimplemented!();
        }

        for h in headers {
            try!(self.encode_header(h, dst));
        }

        Ok(())
    }

    fn encode_header(&mut self, entry: Entry, dst: &mut BytesMut)
        -> Result<(), EncoderError>
    {
        if is_sensitive(&e) {
            unimplemented!();
        }

        /*
        match self.table.entry(name, val) {
            Entry::Indexed(idx) => {
                unimplemented!();
            }
            Entry::Name(idx) => {
                unimplemented!();
            }
            Entry::NotIndexed => {
                unimplemented!();
            }
        }
        */

        unimplemented!();
    }
}

fn is_sensitive(e: &Entry) -> bool {
    false
}
