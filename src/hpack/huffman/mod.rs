mod table;

use self::table::{DECODE_TABLE, ENCODE_TABLE};
use crate::hpack::DecoderError;

use bytes::{BufMut, BytesMut};

// Constants for the byte-wide fallback tables in the generated `table.rs`.
const BRANCH: u16 = 0x8000;
const TABLE_INDEX_MASK: u16 = 0x7f00;
const TABLE_WIDTH: usize = 256;

// Width of the primary lookup table index. The most common HPACK codes are
// five to seven bits long, so twelve bits usually cover two whole symbols.
const FAST_BITS: usize = 12;

// Primary lookup table. Entry layout (u32):
//
//   bits  0..8   first decoded byte
//   bits  8..16  second decoded byte (if any)
//   bits 16..24  symbol count: 0 = code longer than FAST_BITS bits, 1, or 2
//   bits 24..32  total bits consumed by the emitted symbols
const FAST_TABLE: [u32; 1 << FAST_BITS] = build_fast_table();

const fn build_fast_table() -> [u32; 1 << FAST_BITS] {
    let mut table = [0u32; 1 << FAST_BITS];

    // Huffman codes are prefix free, so a code of length `len` owns every
    // index that starts with it: a range of 1 << (FAST_BITS - len) entries.
    // Fill single-symbol entries first, then overwrite with two-symbol
    // entries wherever a second whole code fits in the remaining bits.
    //
    // This fills ranges per code instead of searching codes per index,
    // keeping the work well within the const-eval step limit of older
    // compilers.
    let mut a = 0usize;
    while a < 256 {
        let (len1, code1) = ENCODE_TABLE[a];
        if len1 <= FAST_BITS {
            let rem = FAST_BITS - len1;
            let base = (code1 as usize) << rem;
            let single = (1u32 << 16) | ((len1 as u32) << 24) | a as u32;
            let mut i = 0usize;
            while i < (1usize << rem) {
                table[base + i] = single;
                i += 1;
            }
        }
        a += 1;
    }

    let mut a = 0usize;
    while a < 256 {
        let (len1, code1) = ENCODE_TABLE[a];
        // The shortest code is five bits, so a second symbol can only
        // follow codes short enough to leave room for one.
        if len1 + 5 <= FAST_BITS {
            let rem = FAST_BITS - len1;
            let mut b = 0usize;
            while b < 256 {
                let (len2, code2) = ENCODE_TABLE[b];
                if len2 <= rem {
                    let rem2 = rem - len2;
                    let base = ((code1 as usize) << rem) | ((code2 as usize) << rem2);
                    let pair = (2u32 << 16)
                        | (((len1 + len2) as u32) << 24)
                        | ((b as u32) << 8)
                        | a as u32;
                    let mut i = 0usize;
                    while i < (1usize << rem2) {
                        table[base + i] = pair;
                        i += 1;
                    }
                }
                b += 1;
            }
        }
        a += 1;
    }

    table
}

// Decodes a Huffman encoded string into the provided buffer.
//
// The decoder keeps a 64-bit buffer with the next unconsumed bit at bit 63
// of `acc`. The top `bits` bits are accounted stream bits; below them the
// buffer may hold valid lookahead bits from a previous wide refill (never
// anything else), which makes the `acc |= word >> bits` refill idempotent.
//
// Each iteration of the hot loop decodes up to two symbols from a single
// FAST_TABLE lookup. Codes longer than FAST_BITS bits are rare (they encode
// control characters and non-ASCII octets) and complete by walking the
// byte-wide tables in DECODE_TABLE, exactly like the previous decoder.
pub fn decode(src: &[u8], buf: &mut BytesMut) -> Result<BytesMut, DecoderError> {
    if src.is_empty() {
        return Ok(buf.split());
    }

    // The shortest code is five bits, so the decoded output is at most
    // src.len() * 8 / 5 bytes. The hot loop below speculatively writes two
    // bytes per emitted symbol pair, touching at most one byte past the
    // decoded length. floor(len * 8 / 5) + 1 <= len * 2 holds for len >= 1,
    // so reserving twice the input length covers both.
    buf.reserve(src.len() << 1);

    let len = src.len();
    let base_len = buf.len();

    let mut acc: u64 = 0;
    let mut bits: usize = 0;
    let mut pos: usize = 0;

    // The spare capacity reserved above, as a raw pointer. `chunk_mut` only
    // reallocates when the buffer is full, which the reservation of at least
    // two bytes for a non-empty input rules out.
    let out_start = buf.chunk_mut().as_mut_ptr();
    let mut out = out_start;

    'outer: loop {
        // Refill the bit buffer, seven bytes at a time when possible.
        if pos + 8 <= len {
            // SAFETY: pos + 8 <= len, so the 8-byte read is in bounds.
            let word = u64::from_be_bytes(unsafe {
                src.as_ptr().add(pos).cast::<[u8; 8]>().read_unaligned()
            });
            acc |= word >> bits;
            pos += (63 - bits) >> 3;
            bits |= 56;
        } else {
            while bits <= 56 && pos < len {
                acc |= (src[pos] as u64) << (56 - bits);
                pos += 1;
                bits += 8;
            }
        }

        while bits >= FAST_BITS {
            let entry = FAST_TABLE[(acc >> (64 - FAST_BITS)) as usize];
            let count = (entry >> 16) & 0xff;

            if count == 0 {
                // Code longer than FAST_BITS bits; the longest code is 30
                // bits, so make sure they are buffered before walking.
                if bits < 30 && pos < len {
                    continue 'outer;
                }
                let mut table = 0usize;
                loop {
                    let e = DECODE_TABLE[table * TABLE_WIDTH + (acc >> 56) as usize];
                    if e & BRANCH == 0 {
                        let used = (e >> 8) as usize;
                        if used > bits {
                            return Err(DecoderError::InvalidHuffmanCode);
                        }
                        // SAFETY: the store is within the capacity reserved
                        // above; see the analysis at the top of the function.
                        unsafe {
                            out.write(e as u8);
                            out = out.add(1);
                        }
                        acc <<= used;
                        bits -= used;
                        break;
                    }
                    if bits < 8 {
                        return Err(DecoderError::InvalidHuffmanCode);
                    }
                    table = ((e & TABLE_INDEX_MASK) >> 8) as usize;
                    if table == 0 {
                        return Err(DecoderError::InvalidHuffmanCode);
                    }
                    acc <<= 8;
                    bits -= 8;
                }
                continue;
            }

            let consumed = (entry >> 24) as usize;
            // SAFETY: speculative 2-byte store within the capacity reserved
            // above, which includes one byte of slack past the maximum
            // decoded length; see the analysis at the top of the function.
            unsafe {
                out.write(entry as u8);
                out.add(1).write((entry >> 8) as u8);
                out = out.add(count as usize);
            }
            acc <<= consumed;
            bits -= consumed;
        }

        if pos >= len {
            break;
        }
    }

    // Tail: fewer than FAST_BITS bits remain and the input is exhausted.
    // Note that the unaccounted low bits of `acc` may hold lookahead rather
    // than zeroes. This is harmless: a leaf is only trusted when its code
    // fits in the remaining accounted bits, and byte-wide tables replicate
    // such codes across every suffix, so the lookahead cannot change which
    // symbol is found.
    while bits > 0 {
        // A prefix of the EOS code (all ones, at most 7 bits) is valid
        // padding at a symbol boundary.
        if bits < 8 && (acc >> (64 - bits)) == (1u64 << bits) - 1 {
            break;
        }

        let mut table = 0usize;
        loop {
            let e = DECODE_TABLE[table * TABLE_WIDTH + (acc >> 56) as usize];
            if e & BRANCH == 0 {
                let used = (e >> 8) as usize;
                if used > bits {
                    return Err(DecoderError::InvalidHuffmanCode);
                }
                // SAFETY: the store is within the capacity reserved above;
                // see the analysis at the top of the function.
                unsafe {
                    out.write(e as u8);
                    out = out.add(1);
                }
                acc <<= used;
                bits -= used;
                break;
            }
            if bits < 8 {
                return Err(DecoderError::InvalidHuffmanCode);
            }
            table = ((e & TABLE_INDEX_MASK) >> 8) as usize;
            if table == 0 {
                return Err(DecoderError::InvalidHuffmanCode);
            }
            acc <<= 8;
            bits -= 8;
        }
    }

    // SAFETY: `out` only ever advances from `out_start` within the same
    // reserved allocation.
    let written = unsafe { out.offset_from(out_start) } as usize;
    // SAFETY: `written` bytes were initialized in the spare capacity above.
    unsafe {
        buf.set_len(base_len + written);
    }
    Ok(buf.split())
}

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

    fn decode(src: &[u8]) -> Result<BytesMut, DecoderError> {
        let mut buf = BytesMut::new();
        super::decode(src, &mut buf)
    }

    // The byte-at-a-time decoder this implementation replaced, kept as a
    // reference for differential testing.
    fn reference_decode(src: &[u8]) -> Result<BytesMut, DecoderError> {
        let mut buf = BytesMut::with_capacity(src.len() << 1);

        let mut table = 0;
        let mut acc = 0u32;
        let mut bits = 0;

        for &byte in src {
            acc = (acc << 8) | byte as u32;
            bits += 8;

            while bits >= 8 {
                let index = (acc >> (bits - 8)) as u8 as usize;
                let entry = DECODE_TABLE[table * TABLE_WIDTH + index];

                if entry & BRANCH == 0 {
                    buf.put_u8(entry as u8);
                    table = 0;
                    bits -= (entry >> 8) as usize;
                } else {
                    table = ((entry & TABLE_INDEX_MASK) >> 8) as usize;
                    if table == 0 {
                        return Err(DecoderError::InvalidHuffmanCode);
                    }
                    bits -= 8;
                }
            }
        }

        while bits > 0 {
            debug_assert!(bits < 8);
            let padding = (1u32 << bits) - 1;
            if table == 0 && acc & padding == padding {
                break;
            }

            let index = (acc << (8 - bits)) as u8 as usize;
            let entry = DECODE_TABLE[table * TABLE_WIDTH + index];
            if entry & BRANCH != 0 {
                return Err(DecoderError::InvalidHuffmanCode);
            }

            let used = (entry >> 8) as usize;
            if used > bits {
                return Err(DecoderError::InvalidHuffmanCode);
            }

            buf.put_u8(entry as u8);
            table = 0;
            bits -= used;
        }

        if table == 0 {
            Ok(buf.split())
        } else {
            Err(DecoderError::InvalidHuffmanCode)
        }
    }

    #[test]
    fn decode_single_byte() {
        assert_eq!("o", decode(&[0b00111111]).unwrap());
        assert_eq!("0", decode(&[7]).unwrap());
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

    #[test]
    fn encode_decode_str() {
        const DATA: &[&str] = &[
            "hello world",
            ":method",
            ":scheme",
            ":authority",
            "yahoo.co.jp",
            "GET",
            "http",
            ":path",
            "/images/top/sp2/cmn/logo-ns-130528.png",
            "example.com",
            "hpack-test",
            "xxxxxxx1",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.8; rv:16.0) Gecko/20100101 Firefox/16.0",
            "accept",
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            "cookie",
            "B=76j09a189a6h4&b=3&s=0b",
            "TE",
            "Lorem ipsum dolor sit amet, consectetur adipiscing elit. Morbi non bibendum libero. \
             Etiam ultrices lorem ut.",
        ];

        for s in DATA {
            let mut dst = BytesMut::with_capacity(s.len());

            encode(s.as_bytes(), &mut dst);

            let decoded = decode(&dst).unwrap();

            assert_eq!(&decoded[..], s.as_bytes());
        }
    }

    #[test]
    fn encode_decode_u8() {
        const DATA: &[&[u8]] = &[b"\0", b"\0\0\0", b"\0\x01\x02\x03\x04\x05", b"\xFF\xF8"];

        for s in DATA {
            let mut dst = BytesMut::with_capacity(s.len());

            encode(s, &mut dst);

            let decoded = decode(&dst).unwrap();

            assert_eq!(&decoded[..], &s[..]);
        }
    }

    #[test]
    fn encode_decode_all_octets() {
        let src: Vec<_> = (0..=u8::MAX).collect();
        let mut encoded = BytesMut::new();
        encode(&src, &mut encoded);
        assert_eq!(decode(&encoded).unwrap(), src);
    }

    #[test]
    fn matches_reference_on_valid_input() {
        // Round-trip strings of random bytes of every length.
        let mut rng: u64 = 0x123456789abcdef;
        let mut next = move || {
            rng = rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (rng >> 33) as u8
        };

        for len in 0..600 {
            let src: Vec<u8> = (0..len).map(|_| next()).collect();
            let mut encoded = BytesMut::new();
            encode(&src, &mut encoded);
            assert_eq!(reference_decode(&encoded), decode(&encoded), "len={}", len);
            assert_eq!(&decode(&encoded).unwrap()[..], &src[..], "len={}", len);
        }
    }

    #[test]
    fn matches_reference_on_arbitrary_bytes() {
        // Random (mostly invalid) byte strings must produce identical
        // results, including identical errors.
        let mut rng: u64 = 0xdeadbeefcafe;
        let mut next = move || {
            rng = rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (rng >> 33) as u8
        };

        for _ in 0..20_000 {
            let len = (next() as usize) % 40;
            let src: Vec<u8> = (0..len).map(|_| next()).collect();
            assert_eq!(reference_decode(&src), decode(&src), "src={:?}", src);
        }

        // Bias toward high bytes to exercise long codes and EOS prefixes.
        for _ in 0..20_000 {
            let len = (next() as usize) % 40;
            let src: Vec<u8> = (0..len).map(|_| next() | 0xe0).collect();
            assert_eq!(reference_decode(&src), decode(&src), "src={:?}", src);
        }
    }

    #[test]
    fn rejects_eos_and_invalid_padding() {
        assert_eq!(decode(&[0xff]), Err(DecoderError::InvalidHuffmanCode));
        assert_eq!(
            decode(&[0xff, 0xff, 0xff, 0xff]),
            Err(DecoderError::InvalidHuffmanCode)
        );
        assert_eq!(decode(&[0]), Err(DecoderError::InvalidHuffmanCode));
    }
}

/*
// uncomment to run benchmarks
#[cfg(test)]
mod bench {
    extern crate test;

    use self::test::{black_box, Bencher};
    use super::*;

    fn decode_input(b: &mut Bencher, input: &[u8]) {
        let mut encoded = BytesMut::new();
        encode(input, &mut encoded);

        let mut scratch = BytesMut::with_capacity(input.len() * 2);
        b.bytes = encoded.len() as u64;
        b.iter(|| {
            let decoded = decode(black_box(encoded.as_ref()), &mut scratch).unwrap();
            black_box(decoded);
        });
    }

    #[bench]
    fn decode_short_ascii(b: &mut Bencher) {
        decode_input(b, b"www.example.com");
    }

    #[bench]
    fn decode_header_value(b: &mut Bencher) {
        decode_input(
            b,
            b"text/html,application/xhtml+xml,application/xml;q=0.9;q=0.8",
        );
    }

    #[bench]
    fn decode_long_ascii(b: &mut Bencher) {
        decode_input(
            b,
            b"Mozilla/5.0 (Macintosh; Intel Mac OS X 10.8; rv:16.0) Gecko/20100101 Firefox/16.0",
        );
    }

    #[bench]
    fn decode_all_octets(b: &mut Bencher) {
        let input: Vec<_> = (0..=u8::MAX).collect();
        decode_input(b, &input);
    }
}
*/
