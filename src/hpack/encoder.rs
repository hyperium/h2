use super::{huffman, Header};
use super::table::{Table, Index};

use http::header::{HeaderName, HeaderValue};
use bytes::{BytesMut, BufMut};

#[derive(Debug)]
pub struct Encoder {
    table: Table,
    size_update: Option<SizeUpdate>,
}

#[derive(Debug)]
pub enum Encode {
    Full,
    Partial(EncodeState),
}

#[derive(Debug)]
pub struct EncodeState {
    index: Index,
    value: Option<HeaderValue>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum EncoderError {
    BufferOverflow,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum SizeUpdate {
    One(usize),
    Two(usize, usize), // min, max
}

impl Encoder {
    pub fn new(max_size: usize, capacity: usize) -> Encoder {
        Encoder {
            table: Table::new(max_size, capacity),
            size_update: None,
        }
    }

    /// Queues a max size update.
    ///
    /// The next call to `encode` will include a dynamic size update frame.
    pub fn update_max_size(&mut self, val: usize) {
        match self.size_update {
            Some(SizeUpdate::One(old)) => {
                if val > old {
                    if old > self.table.max_size() {
                        self.size_update = Some(SizeUpdate::One(val));
                    } else {
                        self.size_update = Some(SizeUpdate::Two(old, val));
                    }
                } else {
                    self.size_update = Some(SizeUpdate::One(val));
                }
            }
            Some(SizeUpdate::Two(min, _)) => {
                if val < min {
                    self.size_update = Some(SizeUpdate::One(val));
                } else {
                    self.size_update = Some(SizeUpdate::Two(min, val));
                }
            }
            None => {
                if val != self.table.max_size() {
                    // Don't bother writing a frame if the value already matches
                    // the table's max size.
                    self.size_update = Some(SizeUpdate::One(val));
                }
            }
        }
    }

    /// Encode a set of headers into the provide buffer
    pub fn encode<I>(&mut self, resume: Option<EncodeState>, headers: &mut I, dst: &mut BytesMut)
        -> Encode
        where I: Iterator<Item=Header<Option<HeaderName>>>,
    {
        let len = dst.len();

        if let Err(e) = self.encode_size_updates(dst) {
            if e == EncoderError::BufferOverflow {
                dst.truncate(len);
            }

            unreachable!();
        }

        if let Some(resume) = resume {
            let len = dst.len();

            let res = match resume.value {
                Some(ref value) => {
                    self.encode_header_without_name(
                        &resume.index,
                        value,
                        dst)
                }
                None => {
                    self.encode_header(&resume.index, dst)
                }
            };

            if res.is_err() {
                dst.truncate(len);
                return Encode::Partial(resume);
            }
        }

        let mut last_index = None;

        for header in headers {
            let len = dst.len();

            match header.reify() {
                // The header has an associated name. In which case, try to
                // index it in the table.
                Ok(header) => {
                    let index = self.table.index(header);
                    let res = self.encode_header(&index, dst);

                    if res.is_err() {
                        dst.truncate(len);
                        return Encode::Partial(EncodeState {
                            index: index,
                            value: None,
                        });
                    }

                    last_index = Some(index);
                }
                // The header does not have an associated name. This means that
                // the name is the same as the previously yielded header. In
                // which case, we skip table lookup and just use the same index
                // as the previous entry.
                Err(value) => {
                    let res = self.encode_header_without_name(
                        last_index.as_ref().unwrap(),
                        &value,
                        dst);

                    if res.is_err() {
                        dst.truncate(len);
                        return Encode::Partial(EncodeState {
                            index: last_index.unwrap(),
                            value: Some(value),
                        });
                    }
                }
            };
        }

        Encode::Full
    }

    fn encode_size_updates(&mut self, dst: &mut BytesMut) -> Result<(), EncoderError> {
        match self.size_update.take() {
            Some(SizeUpdate::One(val)) => {
                self.table.resize(val);
                try!(encode_size_update(val, dst));
            }
            Some(SizeUpdate::Two(min, max)) => {
                self.table.resize(min);
                self.table.resize(max);
                try!(encode_size_update(min, dst));
                try!(encode_size_update(max, dst));
            }
            None => {}
        }

        Ok(())
    }

    fn encode_header(&mut self, index: &Index, dst: &mut BytesMut)
        -> Result<(), EncoderError>
    {
        match *index {
            Index::Indexed(idx, _) => {
                try!(encode_int(idx, 7, 0x80, dst));
            }
            Index::Name(idx, _) => {
                let header = self.table.resolve(&index);

                try!(encode_not_indexed(
                        idx,
                        header.value_slice(),
                        header.is_sensitive(),
                        dst));
            }
            Index::Inserted(_) => {
                let header = self.table.resolve(&index);

                assert!(!header.is_sensitive());

                if !dst.has_remaining_mut() {
                    return Err(EncoderError::BufferOverflow);
                }

                dst.put_u8(0b01000000);

                try!(encode_str(header.name().as_slice(), dst));
                try!(encode_str(header.value_slice(), dst));
            }
            Index::InsertedValue(idx, _) => {
                let header = self.table.resolve(&index);

                assert!(!header.is_sensitive());

                try!(encode_int(idx, 6, 0b01000000, dst));
                try!(encode_str(header.value_slice(), dst));
            }
            Index::NotIndexed(_) => {
                let header = self.table.resolve(&index);

                try!(encode_not_indexed2(
                        header.name().as_slice(),
                        header.value_slice(),
                        header.is_sensitive(),
                        dst));
            }
        }

        Ok(())
    }

    fn encode_header_without_name(&mut self, last: &Index,
                                  value: &HeaderValue, dst: &mut BytesMut)
        -> Result<(), EncoderError>
    {
        match *last {
            Index::Indexed(..) |
                Index::Name(..) |
                Index::Inserted(..) |
                Index::InsertedValue(..) =>
            {
                let idx = self.table.resolve_idx(last);

                try!(encode_not_indexed(
                        idx,
                        value.as_ref(),
                        value.is_sensitive(),
                        dst));
            }
            Index::NotIndexed(_) => {
                let last = self.table.resolve(last);

                try!(encode_not_indexed2(
                        last.name().as_slice(),
                        value.as_ref(),
                        value.is_sensitive(),
                        dst));
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

fn encode_size_update<B: BufMut>(val: usize, dst: &mut B) -> Result<(), EncoderError> {
    encode_int(val, 5, 0b00100000, dst)
}

fn encode_not_indexed(name: usize, value: &[u8],
                      sensitive: bool, dst: &mut BytesMut)
    -> Result<(), EncoderError>
{
    if sensitive {
        try!(encode_int(name, 4, 0b10000, dst));
    } else {
        try!(encode_int(name, 4, 0, dst));
    }

    try!(encode_str(value, dst));
    Ok(())
}

fn encode_not_indexed2(name: &[u8], value: &[u8],
                       sensitive: bool, dst: &mut BytesMut)
    -> Result<(), EncoderError>
{
    if !dst.has_remaining_mut() {
        return Err(EncoderError::BufferOverflow);
    }

    if sensitive {
        dst.put_u8(0b10000);
    } else {
        dst.put_u8(0);
    }

    try!(encode_str(name, dst));
    try!(encode_str(value, dst));
    Ok(())
}

fn encode_str(val: &[u8], dst: &mut BytesMut) -> Result<(), EncoderError> {
    use std::io::Cursor;

    if !dst.has_remaining_mut() {
        return Err(EncoderError::BufferOverflow);
    }

    if val.len() != 0 {
        let idx = dst.len();

        // Push a placeholder byte for the length header
        dst.put_u8(0);

        // Encode with huffman
        try!(huffman::encode(val, dst));

        let huff_len = dst.len() - (idx + 1);

        if encode_int_one_byte(huff_len, 7) {
            // Write the string head
            dst[idx] = 0x80 | huff_len as u8;
        } else {
            // Write the head to a placeholer
            let mut buf = [0; 8];

            let head_len = {
                let mut head_dst = Cursor::new(&mut buf);
                try!(encode_int(huff_len, 7, 0x80, &mut head_dst));
                head_dst.position() as usize
            };

            if dst.remaining_mut() < head_len {
                return Err(EncoderError::BufferOverflow);
            }

            // This is just done to reserve space in the destination
            dst.put_slice(&buf[1..head_len]);

            // Shift the header forward
            for i in 0..huff_len {
                let src_i = idx + 1 + (huff_len - (i+1));
                let dst_i = idx + head_len + (huff_len - (i+1));
                dst[dst_i] = dst[src_i];
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

    Ok(())
}

/// Encode an integer into the given destination buffer
fn encode_int<B: BufMut>(
    mut value: usize,   // The integer to encode
    prefix_bits: usize, // The number of bits in the prefix
    first_byte: u8,     // The base upon which to start encoding the int
    dst: &mut B)        // The destination buffer
    -> Result<(), EncoderError>
{
    let mut rem = dst.remaining_mut();

    if rem == 0 {
        return Err(EncoderError::BufferOverflow);
    }

    if encode_int_one_byte(value, prefix_bits) {
        dst.put_u8(first_byte | value as u8);
        return Ok(());
    }

    let low = (1 << prefix_bits) - 1;

    value -= low;

    if value > 0x0fffffff {
        panic!("value out of range");
    }

    dst.put_u8(first_byte | low as u8);
    rem -= 1;

    while value >= 128 {
        if rem == 0 {
            return Err(EncoderError::BufferOverflow);
        }

        dst.put_u8(0b10000000 | value as u8);
        rem -= 1;

        value = value >> 7;
    }

    if rem == 0 {
        return Err(EncoderError::BufferOverflow);
    }

    dst.put_u8(value as u8);

    Ok(())
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
            assert_eq!(0x80 | (61 + (65-i)), res[0]);
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
        use http::header::HeaderValue;

        let name = "my-password".parse().unwrap();
        let mut value = HeaderValue::try_from_bytes(b"12345").unwrap();
        value.set_sensitive(true);

        let header = Header::Field { name: Some(name), value: value };

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

        let header = Header::Field { name: Some(name), value: value };

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

        let header = Header::Field { name: Some(name), value: value };
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
    fn test_encoding_headers_with_same_name() {
        let mut encoder = Encoder::default();
        let name = "hello";

        // Encode first one
        let _ = encode(&mut encoder, vec![header(name, "one")]);

        // Encode second one
        let res = encode(&mut encoder, vec![header(name, "two")]);
        assert_eq!(&[0x40 | 62, 0x80 | 3], &res[0..2]);
        assert_eq!("two", huff_decode(&res[2..]));
        assert_eq!(5, res.len());

        // Encode the first one again
        let res = encode(&mut encoder, vec![header(name, "one")]);
        assert_eq!(&[0x80 | 63], &res[..]);

        // Now the second one
        let res = encode(&mut encoder, vec![header(name, "two")]);
        assert_eq!(&[0x80 | 62], &res[..]);
    }

    #[test]
    fn test_evicting_headers_when_multiple_of_same_name_are_in_table() {
        // The encoder only has space for 2 headers
        let mut encoder = Encoder::new(76, 0);

        let _ = encode(&mut encoder, vec![header("foo", "bar")]);
        assert_eq!(1, encoder.table.len());

        let _ = encode(&mut encoder, vec![header("bar", "foo")]);
        assert_eq!(2, encoder.table.len());

        // This will evict the first header, while still referencing the header
        // name
        let res = encode(&mut encoder, vec![header("foo", "baz")]);
        assert_eq!(&[0x40 | 63, 0, 0x80 | 3], &res[..3]);
        assert_eq!(2, encoder.table.len());

        // Try adding the same header again
        let res = encode(&mut encoder, vec![header("foo", "baz")]);
        assert_eq!(&[0x80 | 62], &res[..]);
        assert_eq!(2, encoder.table.len());
    }

    #[test]
    fn test_max_size_zero() {
        // Static table only
        let mut encoder = Encoder::new(0, 0);
        let res = encode(&mut encoder, vec![method("GET")]);
        assert_eq!(*res, [0x80 | 2]);
        assert_eq!(encoder.table.len(), 0);

        let res = encode(&mut encoder, vec![header("foo", "bar")]);
        assert_eq!(&[0, 0x80 | 2], &res[..2]);
        assert_eq!("foo", huff_decode(&res[2..4]));
        assert_eq!(0x80 | 3, res[4]);
        assert_eq!("bar", huff_decode(&res[5..8]));
        assert_eq!(0, encoder.table.len());

        // Encode a custom value
        let res = encode(&mut encoder, vec![header("transfer-encoding", "chunked")]);
        assert_eq!(&[15, 42, 0x80 | 6], &res[..3]);
        assert_eq!("chunked", huff_decode(&res[3..]));
    }

    #[test]
    fn test_update_max_size_combos() {
        let mut encoder = Encoder::default();
        assert!(encoder.size_update.is_none());
        assert_eq!(4096, encoder.table.max_size());

        encoder.update_max_size(4096); // Default size
        assert!(encoder.size_update.is_none());

        encoder.update_max_size(0);
        assert_eq!(Some(SizeUpdate::One(0)), encoder.size_update);

        encoder.update_max_size(100);
        assert_eq!(Some(SizeUpdate::Two(0, 100)), encoder.size_update);

        let mut encoder = Encoder::default();
        encoder.update_max_size(8000);
        assert_eq!(Some(SizeUpdate::One(8000)), encoder.size_update);

        encoder.update_max_size(100);
        assert_eq!(Some(SizeUpdate::One(100)), encoder.size_update);

        encoder.update_max_size(8000);
        assert_eq!(Some(SizeUpdate::Two(100, 8000)), encoder.size_update);

        encoder.update_max_size(4000);
        assert_eq!(Some(SizeUpdate::Two(100, 4000)), encoder.size_update);

        encoder.update_max_size(50);
        assert_eq!(Some(SizeUpdate::One(50)), encoder.size_update);
    }

    #[test]
    fn test_resizing_table() {
        let mut encoder = Encoder::default();

        // Add a header
        let _ = encode(&mut encoder, vec![header("foo", "bar")]);

        encoder.update_max_size(1);
        assert_eq!(1, encoder.table.len());

        let res = encode(&mut encoder, vec![method("GET")]);
        assert_eq!(&[32 | 1, 0x80 | 2], &res[..]);
        assert_eq!(0, encoder.table.len());

        let res = encode(&mut encoder, vec![header("foo", "bar")]);
        assert_eq!(0, res[0]);

        encoder.update_max_size(100);
        let res = encode(&mut encoder, vec![header("foo", "bar")]);
        assert_eq!(&[32 | 31, 69, 64], &res[..3]);

        encoder.update_max_size(0);
        let res = encode(&mut encoder, vec![header("foo", "bar")]);
        assert_eq!(&[32, 0], &res[..2]);
    }

    #[test]
    fn test_decreasing_table_size_without_eviction() {
        let mut encoder = Encoder::default();

        // Add a header
        let _ = encode(&mut encoder, vec![header("foo", "bar")]);

        encoder.update_max_size(100);
        assert_eq!(1, encoder.table.len());

        let res = encode(&mut encoder, vec![header("foo", "bar")]);
        assert_eq!(&[32 | 31, 69, 0x80 | 62], &res[..]);
    }

    #[test]
    fn test_nameless_header() {
        let mut encoder = Encoder::default();

        let res = encode(&mut encoder, vec![
            Header::Field {
                name: Some("hello".parse().unwrap()),
                value: HeaderValue::try_from_bytes(b"world").unwrap(),
            },
            Header::Field {
                name: None,
                value: HeaderValue::try_from_bytes(b"zomg").unwrap(),
            },
        ]);

        assert_eq!(&[0x40, 0x80 | 4], &res[0..2]);
        assert_eq!("hello", huff_decode(&res[2..6]));
        assert_eq!(0x80 | 4, res[6]);
        assert_eq!("world", huff_decode(&res[7..11]));

        // Next is not indexed
        assert_eq!(&[15, 47, 0x80 | 3], &res[11..14]);
        assert_eq!("zomg", huff_decode(&res[14..]));
    }

    #[test]
    fn test_nameless_header_at_resume() {
        let mut encoder = Encoder::default();
        let mut dst = BytesMut::from(Vec::with_capacity(11));

        let mut input = vec![
            Header::Field {
                name: Some("hello".parse().unwrap()),
                value: HeaderValue::try_from_bytes(b"world").unwrap(),
            },
            Header::Field {
                name: None,
                value: HeaderValue::try_from_bytes(b"zomg").unwrap(),
            },
        ].into_iter();

        let resume = match encoder.encode(None, &mut input, &mut dst) {
            Encode::Partial(r) => r,
            _ => panic!(),
        };

        assert_eq!(&[0x40, 0x80 | 4], &dst[0..2]);
        assert_eq!("hello", huff_decode(&dst[2..6]));
        assert_eq!(0x80 | 4, dst[6]);
        assert_eq!("world", huff_decode(&dst[7..11]));

        dst.clear();

        match encoder.encode(Some(resume), &mut input, &mut dst) {
            Encode::Full => {}
            _ => panic!(),
        }

        // Next is not indexed
        assert_eq!(&[15, 47, 0x80 | 3], &dst[0..3]);
        assert_eq!("zomg", huff_decode(&dst[3..]));
    }

    #[test]
    #[ignore]
    fn test_evicted_overflow() {
        // Not sure what the best way to do this is.
    }

    fn encode(e: &mut Encoder, hdrs: Vec<Header<Option<HeaderName>>>) -> BytesMut {
        let mut dst = BytesMut::with_capacity(1024);
        e.encode(None, &mut hdrs.into_iter(), &mut dst);
        dst
    }

    fn method(s: &str) -> Header<Option<HeaderName>> {
        Header::Method(Method::from_bytes(s.as_bytes()).unwrap())
    }

    fn header(name: &str, val: &str) -> Header<Option<HeaderName>> {
        use http::header::{HeaderName, HeaderValue};

        let name = HeaderName::from_bytes(name.as_bytes()).unwrap();
        let value = HeaderValue::try_from_bytes(val.as_bytes()).unwrap();

        Header::Field { name: Some(name), value: value }
    }

    fn huff_decode(src: &[u8]) -> BytesMut {
        let mut buf = BytesMut::new();
        huffman::decode(src, &mut buf).unwrap()
    }
}
