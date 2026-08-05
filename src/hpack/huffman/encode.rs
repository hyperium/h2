use crate::hpack::huffman::table::ENCODE_TABLE;

use bytes::{BufMut, BytesMut};

pub fn encode(src: &[u8], dst: &mut BytesMut) {
    let mut bits: u64 = 0;
    let mut bits_left = 40;

    for &b in src {
        let (nbits, code) = ENCODE_TABLE[b as usize];

        bits |= code << (bits_left - nbits);
        bits_left -= nbits;

        while bits_left <= 32 {
            dst.put_u8((bits >> 32) as u8);

            bits <<= 8;
            bits_left += 8;
        }
    }

    if bits_left != 40 {
        // This writes the EOS token
        bits |= (1 << bits_left) - 1;
        dst.put_u8((bits >> 32) as u8);
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn encode_single_byte() {
        let mut dst = BytesMut::with_capacity(1);

        encode(b"o", &mut dst);
        assert_eq!(&dst[..], &[0b00111111]);

        dst.clear();
        encode(b"0", &mut dst);
        assert_eq!(&dst[..], &[7]);

        dst.clear();
        encode(b"A", &mut dst);
        assert_eq!(&dst[..], &[(0x21 << 2) + 3]);
    }
}
