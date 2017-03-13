mod table;

use self::table::DECODE_TABLE;
use hpack::DecoderError;

use bytes::{BytesMut, BufMut};

// Constructed in the generated `table.rs` file
struct Decoder {
    state: usize,
    maybe_eos: bool,
}

// These flags must match the ones in genhuff.rs

const MAYBE_EOS: u8 = 1;
const DECODED: u8 = 2;
const ERROR: u8 = 4;

pub fn decode(src: &[u8]) -> Result<BytesMut, DecoderError> {
    let mut decoder = Decoder::new();

    // Max compression ration is >= 0.5
    let mut dst = BytesMut::with_capacity(src.len() << 1);

    for b in src {
        if let Some(b) = try!(decoder.decode4(b >> 4)) {
            dst.put_u8(b);
        }

        if let Some(b) = try!(decoder.decode4(b & 0xf)) {
            dst.put_u8(b);
        }
    }

    if !decoder.maybe_eos {
        // TODO: handle error
        unimplemented!();
    }

    Ok(dst)
}

impl Decoder {
    fn new() -> Decoder {
        Decoder {
            state: 0,
            maybe_eos: false,
        }
    }

    // Decodes 4 bits
    fn decode4(&mut self, input: u8) -> Result<Option<u8>, DecoderError> {
        // (next-state, byte, flags)
        let (next, byte, flags) = DECODE_TABLE[self.state][input as usize];

        if flags & ERROR == ERROR {
            // Data followed the EOS marker
            unimplemented!();
        }

        let mut ret = None;

        if flags & DECODED == DECODED {
            ret = Some(byte);
        }

        self.state = next;
        self.maybe_eos = flags & MAYBE_EOS == MAYBE_EOS;

        Ok(ret)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn decode_single_byte() {
        let buf = [0b00111111];

        let actual = decode(&buf).unwrap();
        assert_eq!(actual, "o");

        /*
        let mut decoder = HuffmanDecoder::new();
        // (The + (2^n - 1) at the final byte is to add the correct expected
        // padding: 1s)
        {
            // We need to shift it by 3, since we need the top-order bytes to
            // start the code point.
            let hex_buffer = [(0x7 << 3) + 7];
            let expected_result = vec![b'o'];

            let result = decoder.decode(&hex_buffer).ok().unwrap();

            assert_eq!(result, expected_result);
        }
        {
            let hex_buffer = [0x0 + 7];
            let expected_result = vec![b'0'];

            let result = decoder.decode(&hex_buffer).ok().unwrap();

            assert_eq!(result, expected_result);
        }
        {
            // The length of the codepoint is 6, so we shift by two
            let hex_buffer = [(0x21 << 2) + 3];
            let expected_result = vec![b'A'];

            let result = decoder.decode(&hex_buffer).ok().unwrap();

            assert_eq!(result, expected_result);
        }
        */
    }
}
