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
    pub fn new(max_size: usize, capacity: usize) -> Encoder {
        Encoder {
            table: Table::new(max_size, capacity),
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
        match self.table.index(header) {
            Index::Indexed(idx, header) => {
                assert!(!header.is_sensitive());
                encode_int(idx, 7, 0x80, dst);
            }
            Index::Name(idx, header) => {
                if header.is_sensitive() {
                    encode_int(idx, 4, 0b10000, dst);
                } else {
                    encode_int(idx, 4, 0, dst);
                }

                encode_str(header.value_slice(), dst);
            }
            Index::Inserted(header) => {
                assert!(!header.is_sensitive());
                dst.put_u8(0b01000000);
                encode_str(header.name().as_slice(), dst);
                encode_str(header.value_slice(), dst);
            }
            Index::InsertedValue(idx, header) => {
                assert!(!header.is_sensitive());

                encode_int(idx, 6, 0b01000000, dst);
                encode_str(header.value_slice(), dst);
            }
            Index::NotIndexed(header) => {
                if header.is_sensitive() {
                    dst.put_u8(0b10000);
                } else {
                    dst.put_u8(0);
                }

                encode_str(header.name().as_slice(), dst);
                encode_str(header.value_slice(), dst);
            }
        }

        Ok(())
    }
}

impl Default for Encoder {
    fn default() -> Encoder {
        Encoder::new(4096, 0)
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

#[cfg(test)]
mod test {
    use super::*;
    use hpack::Header;
    use http::*;

    #[test]
    fn test_encode_method_get() {
        let mut encoder = Encoder::default();
        let res = encode(&mut encoder, vec![method("GET")]);
        assert_eq!(*res, [0x80 | 2]);
        assert_eq!(encoder.table.len(), 0);
    }

    #[test]
    fn test_encode_method_post() {
        let mut encoder = Encoder::default();
        let res = encode(&mut encoder, vec![method("POST")]);
        assert_eq!(*res, [0x80 | 3]);
        assert_eq!(encoder.table.len(), 0);
    }

    #[test]
    fn test_encode_method_patch() {
        let mut encoder = Encoder::default();
        let res = encode(&mut encoder, vec![method("PATCH")]);

        assert_eq!(res[0], 0b01000000 | 2); // Incremental indexing w/ name pulled from table
        assert_eq!(res[1], 0x80 | 5);       // header value w/ huffman coding

        assert_eq!("PATCH", huff_decode(&res[2..7]));
        assert_eq!(encoder.table.len(), 1);

        let res = encode(&mut encoder, vec![method("PATCH")]);

        assert_eq!(1 << 7 | 62, res[0]);
        assert_eq!(1, res.len());
    }

    #[test]
    fn test_repeated_headers_are_indexed() {
        let mut encoder = Encoder::default();
        let res = encode(&mut encoder, vec![header("foo", "hello")]);

        assert_eq!(&[0b01000000, 0x80 | 2], &res[0..2]);
        assert_eq!("foo", huff_decode(&res[2..4]));
        assert_eq!(0x80 | 4, res[4]);
        assert_eq!("hello", huff_decode(&res[5..]));
        assert_eq!(9, res.len());

        assert_eq!(1, encoder.table.len());

        let res = encode(&mut encoder, vec![header("foo", "hello")]);
        assert_eq!([0x80 | 62], *res);

        assert_eq!(encoder.table.len(), 1);
    }

    #[test]
    fn test_evicting_headers() {
        let mut encoder = Encoder::default();

        // Fill the table
        for i in 0..64 {
            let key = format!("x-hello-world-{:02}", i);
            let res = encode(&mut encoder, vec![header(&key, &key)]);

            assert_eq!(&[0b01000000, 0x80 | 12], &res[0..2]);
            assert_eq!(key, huff_decode(&res[2..14]));
            assert_eq!(0x80 | 12, res[14]);
            assert_eq!(key, huff_decode(&res[15..]));
            assert_eq!(27, res.len());

            // Make sure the header can be found...
            let res = encode(&mut encoder, vec![header(&key, &key)]);

            // Only check that it is found
            assert_eq!(0x80, res[0] & 0x80);
        }

        assert_eq!(4096, encoder.table.size());
        assert_eq!(64, encoder.table.len());

        // Find existing headers
        for i in 0..64 {
            let key = format!("x-hello-world-{:02}", i);
            let res = encode(&mut encoder, vec![header(&key, &key)]);
            assert_eq!(0x80, res[0] & 0x80);
        }

        // Insert a new header
        let key = "x-hello-world-64";
        let res = encode(&mut encoder, vec![header(key, key)]);

        assert_eq!(&[0b01000000, 0x80 | 12], &res[0..2]);
        assert_eq!(key, huff_decode(&res[2..14]));
        assert_eq!(0x80 | 12, res[14]);
        assert_eq!(key, huff_decode(&res[15..]));
        assert_eq!(27, res.len());

        assert_eq!(64, encoder.table.len());

        // Now try encoding entries that should exist in the table
        for i in 1..65 {
            let key = format!("x-hello-world-{:02}", i);
            let res = encode(&mut encoder, vec![header(&key, &key)]);
            assert_eq!(0x80 | (i + 61), res[0]);
        }
    }

    #[test]
    fn test_large_headers_are_not_indexed() {
        let mut encoder = Encoder::new(128, 0);
        let key = "hello-world-hello-world-HELLO-zzz";

        let res = encode(&mut encoder, vec![header(key, key)]);

        assert_eq!(&[0, 0x80 | 25], &res[..2]);

        assert_eq!(0, encoder.table.len());
        assert_eq!(0, encoder.table.size());
    }

    #[test]
    fn test_sensitive_headers_are_never_indexed() {
        use http::header::{HeaderName, HeaderValue};

        let name = "my-password".parse().unwrap();
        let mut value = HeaderValue::try_from_bytes(b"12345").unwrap();
        value.set_sensitive(true);

        let header = Header::Field { name: name, value: value };

        // Now, try to encode the sensitive header

        let mut encoder = Encoder::default();
        let res = encode(&mut encoder, vec![header]);

        assert_eq!(&[0b10000, 0x80 | 8], &res[..2]);
        assert_eq!("my-password", huff_decode(&res[2..10]));
        assert_eq!(0x80 | 4, res[10]);
        assert_eq!("12345", huff_decode(&res[11..]));

        // Now, try to encode a sensitive header w/ a name in the static table
        let name = "authorization".parse().unwrap();
        let mut value = HeaderValue::try_from_bytes(b"12345").unwrap();
        value.set_sensitive(true);

        let header = Header::Field { name: name, value: value };

        let mut encoder = Encoder::default();
        let res = encode(&mut encoder, vec![header]);

        assert_eq!(&[0b11111, 8], &res[..2]);
        assert_eq!(0x80 | 4, res[2]);
        assert_eq!("12345", huff_decode(&res[3..]));

        // Using the name component of a previously indexed header (without
        // sensitive flag set)

        let _ = encode(&mut encoder, vec![self::header("my-password", "not-so-secret")]);

        let name = "my-password".parse().unwrap();
        let mut value = HeaderValue::try_from_bytes(b"12345").unwrap();
        value.set_sensitive(true);

        let header = Header::Field { name: name, value: value };
        let res = encode(&mut encoder, vec![header]);

        assert_eq!(&[0b11111, 47], &res[..2]);
        assert_eq!(0x80 | 4, res[2]);
        assert_eq!("12345", huff_decode(&res[3..]));
    }

    #[test]
    fn test_content_length_value_not_indexed() {
        let mut encoder = Encoder::default();
        let res = encode(&mut encoder, vec![header("content-length", "1234")]);

        assert_eq!(&[15, 13, 0x80 | 3], &res[0..3]);
        assert_eq!("1234", huff_decode(&res[3..]));
        assert_eq!(6, res.len());
    }

    #[test]
    fn test_at_most_two_values_per_name_indexed() {
    }

    #[test]
    fn test_index_header_with_duplicate_name_does_not_evict() {
    }

    #[test]
    fn test_max_size_zero() {
    }

    #[test]
    fn test_increasing_table_size() {
    }

    #[test]
    fn test_decreasing_table_size_without_eviction() {
    }

    #[test]
    fn test_decreasing_table_size_with_eviction() {
    }

    #[test]
    fn test_encoding_into_undersized_buf() {
        // Test hitting end at multiple points.
    }


    fn encode(e: &mut Encoder, hdrs: Vec<Header>) -> BytesMut {
        let mut dst = BytesMut::with_capacity(1024);
        e.encode(hdrs, &mut dst);
        dst
    }

    fn method(s: &str) -> Header {
        Header::Method(Method::from_bytes(s.as_bytes()).unwrap())
    }

    fn header(name: &str, val: &str) -> Header {
        use http::header::{HeaderName, HeaderValue};

        let name = HeaderName::from_bytes(name.as_bytes()).unwrap();
        let value = HeaderValue::try_from_bytes(val.as_bytes()).unwrap();

        Header::Field { name: name, value: value }
    }

    fn huff_decode(src: &[u8]) -> BytesMut {
        huffman::decode(src).unwrap()
    }
}
