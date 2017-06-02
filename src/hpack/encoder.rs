use super::{huffman, Header};
use super::table::{Table, Index};

use http::header::{HeaderName, HeaderValue};
use bytes::{BytesMut, BufMut};

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
        where I: IntoIterator<Item=Header>,
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

    fn encode_header(&mut self, header: Header, dst: &mut BytesMut)
        -> Result<(), EncoderError>
    {
        if header.is_sensitive() {
            unimplemented!();
        }

        match self.table.index(header) {
            Index::Indexed(idx, _header) => {
                encode_int(idx, 7, 0x80, dst);
            }
            Index::Name(idx, header) => {
                encode_int(idx, 4, 0, dst);
                encode_str(header.value_slice(), dst);
            }
            Index::Inserted(header) => {
                dst.put_u8(0b01000000);
                encode_str(header.name().as_slice(), dst);
                encode_str(header.value_slice(), dst);
            }
            Index::InsertedValue(idx, header) => {
                encode_int(idx, 6, 0b01000000, dst);
                encode_str(header.value_slice(), dst);
            }
            Index::NotIndexed(header) => {
                dst.put_u8(0);
                encode_str(header.name().as_slice(), dst);
                encode_str(header.value_slice(), dst);
            }
        }

        Ok(())
    }
}

fn encode_str(val: &[u8], dst: &mut BytesMut) {
    use std::io::Cursor;

    if val.len() != 0 {
        let idx = dst.len();

        // Push a placeholder byte for the length header
        dst.put_u8(0);

        // Encode with huffman
        huffman::encode(val, dst);

        let huff_len = dst.len() - (idx + 1);

        if encode_int_one_byte(huff_len, 7) {
            // Write the string head
            dst[idx] = (0x80 | huff_len as u8);
        } else {
            // Write the head to a placeholer
            let mut buf = [0; 8];

            let head_len = {
                let mut head_dst = Cursor::new(&mut buf);
                encode_int(huff_len, 7, 0x80, &mut head_dst);
                head_dst.position() as usize
            };

            // This is just done to reserve space in the destination
            dst.put_slice(&buf[1..head_len]);

            // Shift the header forward
            for i in 0..huff_len {
                dst[idx + head_len + (huff_len - i)] = dst[idx + 1 + (huff_len - 1)];
            }

            // Copy in the head
            for i in 0..head_len {
                dst[idx + i] = buf[i];
            }
        }
    } else {
        // Write an empty string
        dst.put_u8(0);
    }
}

/// Encode an integer into the given destination buffer
fn encode_int<B: BufMut>(
    mut value: usize,   // The integer to encode
    prefix_bits: usize, // The number of bits in the prefix
    first_byte: u8,     // The base upon which to start encoding the int
    dst: &mut B)        // The destination buffer
{
    if encode_int_one_byte(value, prefix_bits) {
        dst.put_u8(first_byte | value as u8);
        return;
    }

    let low = (1 << prefix_bits) - 1;

    value -= low;

    if value > 0x0fffffff {
        panic!("value out of range");
    }

    dst.put_u8(first_byte | low as u8);

    while value >= 128 {
        dst.put_u8(0b10000000 | value as u8);
        value = value >> 7;
    }

    dst.put_u8(value as u8);
}

/// Returns true if the in the int can be fully encoded in the first byte.
fn encode_int_one_byte(value: usize, prefix_bits: usize) -> bool {
    value < (1 << prefix_bits) - 1
}
