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

    if !decoder.is_final() {
        return Err(DecoderError::InvalidHuffmanCode);
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

    fn is_final(&self) -> bool {
        self.state == 0 || self.maybe_eos
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn decode_single_byte() {
        assert_eq!("o", decode(&[0b00111111]).unwrap());
        assert_eq!("0", decode(&[0x0 + 7]).unwrap());
        assert_eq!("A", decode(&[(0x21 << 2) + 3]).unwrap());
    }

    #[test]
    fn single_char_multi_byte() {
        assert_eq!("#", decode(&[255, 160 + 15]).unwrap());
        assert_eq!("$", decode(&[255, 200 + 7]).unwrap());
        assert_eq!("\x0a", decode(&[255, 255, 255, 240 + 3]).unwrap());
    }

    #[test]
    fn multi_char() {
        assert_eq!("!0", decode(&[254, 1]).unwrap());
        assert_eq!(" !", decode(&[0b01010011, 0b11111000]).unwrap());
    }
}
