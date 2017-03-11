//! A module exposing utilities for encoding and decoding Huffman-coded octet
//! strings, under the Huffman code defined by HPACK.
//! (HPACK-draft-10, Appendix B)

use std::collections::HashMap;

/// Represents a symbol that can be inserted into a Huffman-encoded octet
/// string.
enum HuffmanCodeSymbol {
    /// Any octet is a valid symbol
    Symbol(u8),
    /// A special symbol represents the end of the string
    EndOfString,
}

impl HuffmanCodeSymbol {
    pub fn new(symbol: usize) -> HuffmanCodeSymbol {
        if symbol == 256 {
            HuffmanCodeSymbol::EndOfString
        } else {
            // It is safe to downcast since now we know that the value
            // is in the half-open interval [0, 256)
            HuffmanCodeSymbol::Symbol(symbol as u8)
        }
    }
}

/// Represents the error variants that the `HuffmanDecoder` can return.
#[derive(PartialEq)]
#[derive(Copy)]
#[derive(Clone)]
#[derive(Debug)]
pub enum HuffmanDecoderError {
    /// Any padding strictly larger than 7 bits MUST be interpreted as an error
    PaddingTooLarge,
    /// Any padding that does not correspond to the most significant bits of
    /// EOS MUST be interpreted as an error.
    InvalidPadding,
    /// If EOS is ever found in the string, it causes an error.
    EOSInString,
}

/// The type that represents the result of the `decode` method of the
/// `HuffmanDecoder`.
pub type HuffmanDecoderResult = Result<Vec<u8>, HuffmanDecoderError>;

/// A simple implementation of a Huffman code decoder.
pub struct HuffmanDecoder {
    table: HashMap<u8, HashMap<u32, HuffmanCodeSymbol>>,
    // The representation of the EOS: the left-aligned code representation and
    // the actual length of the codepoint, as a tuple.
    eos_codepoint: (u32, u8),
}

impl HuffmanDecoder {
    /// Constructs a new `HuffmanDecoder` using the given table of
    /// (code point, code length) tuples to represent the Huffman code.
    fn from_table(table: &[(u32, u8)]) -> HuffmanDecoder {
        if table.len() != 257 {
            panic!("Invalid Huffman code table. It must define exactly 257 symbols.");
        }

        let mut decoder_table: HashMap<u8, HashMap<u32, HuffmanCodeSymbol>> =
            HashMap::new();
        let mut eos_codepoint: Option<(u32, u8)> = None;

        for (symbol, &(code, code_len)) in table.iter().enumerate() {
            if !decoder_table.contains_key(&code_len) {
                decoder_table.insert(code_len, HashMap::new());
            }
            let subtable = decoder_table.get_mut(&code_len).unwrap();
            let huff_symbol = HuffmanCodeSymbol::new(symbol);
            match huff_symbol {
                HuffmanCodeSymbol::EndOfString => {
                    // We also remember the code point of the EOS for easier
                    // reference later on.
                    eos_codepoint = Some((code, code_len));
                },
                _ => {}
            };
            subtable.insert(code, huff_symbol);
        }

        HuffmanDecoder {
            table: decoder_table,
            eos_codepoint: eos_codepoint.unwrap(),
        }
    }

    /// Constructs a new HuffmanDecoder with the default Huffman code table, as
    /// defined in the HPACK-draft-10, Appendix B.
    pub fn new() -> HuffmanDecoder {
        HuffmanDecoder::from_table(HUFFMAN_CODE_TABLE)
    }

    /// Decodes the buffer `buf` into a newly allocated `Vec`.
    ///
    /// It assumes that the entire buffer should be considered as the Huffman
    /// encoding of an octet string and handles the padding rules
    /// accordingly.
    pub fn decode(&mut self, buf: &[u8]) -> HuffmanDecoderResult {
        let mut current: u32 = 0;
        let mut current_len: u8 = 0;
        let mut result: Vec<u8> = Vec::new();

        for b in BitIterator::new(buf.iter()) {
            current_len += 1;
            current <<= 1;
            if b {
                current |= 1;
            }

            if self.table.contains_key(&current_len) {
                let length_table = self.table.get(&current_len).unwrap();
                if length_table.contains_key(&current) {
                    let decoded_symbol = match length_table.get(&current).unwrap() {
                        &HuffmanCodeSymbol::Symbol(symbol) => symbol,
                        &HuffmanCodeSymbol::EndOfString => {
                            // If the EOS symbol is detected within the stream,
                            // we need to consider it an error.
                            return Err(HuffmanDecoderError::EOSInString);
                        },
                    };
                    result.push(decoded_symbol);
                    current = 0;
                    current_len = 0;
                }
            }
        }

        // Now we need to verify that the padding is correct.
        // The spec mandates that the padding must not be strictly longer than
        // 7 bits and that it must represent the most significant bits of the
        // EOS symbol's code.

        // First: the check for the length of the padding
        if current_len > 7 {
            return Err(HuffmanDecoderError::PaddingTooLarge)
        }

        // Second: the padding corresponds to the most-significant bits of the
        // EOS symbol.
        // Align both of them to have their most significant bit as the most
        // significant bit of a u32.
        let right_align_current = if current_len == 0 {
            0
        } else {
            current << (32 - current_len)
        };
        let right_align_eos = self.eos_codepoint.0 << (32 - self.eos_codepoint.1);
        // Now take only the necessary amount of most significant bit of EOS.
        // The mask defines a bit pattern of `current_len` leading set bits,
        // followed by the rest of the bits 0.
        let mask = if current_len == 0 {
            0
        } else {
            ((1 << current_len) - 1) << (32 - current_len)
        };
        // The mask is now used to strip the unwanted bits of the EOS
        let eos_mask = right_align_eos & mask;

        if eos_mask != right_align_current {
            return Err(HuffmanDecoderError::InvalidPadding);
        }

        Ok(result)
    }
}

/// A helper struct that represents an iterator over individual bits of all
/// bytes found in a wrapped Iterator over bytes.
/// Bits are represented as `bool`s, where `true` corresponds to a set bit and
/// `false` to a 0 bit.
///
/// Bits are yielded in order of significance, starting from the
/// most-significant bit.
struct BitIterator<'a, I: Iterator> {
    buffer_iterator: I,
    current_byte: Option<&'a u8>,
    /// The bit-position within the current byte
    pos: u8,
}

impl<'a, I: Iterator> BitIterator<'a, I>
        where I: Iterator<Item=&'a u8> {
    pub fn new(iterator: I) -> BitIterator<'a, I> {
        BitIterator::<'a, I> {
            buffer_iterator: iterator,
            current_byte: None,
            pos: 7,
        }
    }
}

impl<'a, I> Iterator for BitIterator<'a, I>
        where I: Iterator<Item=&'a u8> {
    type Item = bool;

    fn next(&mut self) -> Option<bool> {
        if self.current_byte.is_none() {
            self.current_byte = self.buffer_iterator.next();
            self.pos = 7;
        }

        // If we still have `None`, it means the buffer has been exhausted
        if self.current_byte.is_none() {
            return None;
        }

        let b = *self.current_byte.unwrap();

        let is_set = (b & (1 << self.pos)) == (1 << self.pos);
        if self.pos == 0 {
            // We have exhausted all bits from the current byte -- try to get
            // a new one on the next pass.
            self.current_byte = None;
        } else {
            // Still more bits left here...
            self.pos -= 1;
        }

        Some(is_set)
    }
}

static HUFFMAN_CODE_TABLE: &'static [(u32, u8)] = &[
    (0x1ff8, 13),
    (0x7fffd8, 23),
    (0xfffffe2, 28),
    (0xfffffe3, 28),
    (0xfffffe4, 28),
    (0xfffffe5, 28),
    (0xfffffe6, 28),
    (0xfffffe7, 28),
    (0xfffffe8, 28),
    (0xffffea, 24),
    (0x3ffffffc, 30),
    (0xfffffe9, 28),
    (0xfffffea, 28),
    (0x3ffffffd, 30),
    (0xfffffeb, 28),
    (0xfffffec, 28),
    (0xfffffed, 28),
    (0xfffffee, 28),
    (0xfffffef, 28),
    (0xffffff0, 28),
    (0xffffff1, 28),
    (0xffffff2, 28),
    (0x3ffffffe, 30),
    (0xffffff3, 28),
    (0xffffff4, 28),
    (0xffffff5, 28),
    (0xffffff6, 28),
    (0xffffff7, 28),
    (0xffffff8, 28),
    (0xffffff9, 28),
    (0xffffffa, 28),
    (0xffffffb, 28),
    (0x14, 6),
    (0x3f8, 10),
    (0x3f9, 10),
    (0xffa, 12),
    (0x1ff9, 13),
    (0x15, 6),
    (0xf8, 8),
    (0x7fa, 11),
    (0x3fa, 10),
    (0x3fb, 10),
    (0xf9, 8),
    (0x7fb, 11),
    (0xfa, 8),
    (0x16, 6),
    (0x17, 6),
    (0x18, 6),
    (0x0, 5),
    (0x1, 5),
    (0x2, 5),
    (0x19, 6),
    (0x1a, 6),
    (0x1b, 6),
    (0x1c, 6),
    (0x1d, 6),
    (0x1e, 6),
    (0x1f, 6),
    (0x5c, 7),
    (0xfb, 8),
    (0x7ffc, 15),
    (0x20, 6),
    (0xffb, 12),
    (0x3fc, 10),
    (0x1ffa, 13),
    (0x21, 6),
    (0x5d, 7),
    (0x5e, 7),
    (0x5f, 7),
    (0x60, 7),
    (0x61, 7),
    (0x62, 7),
    (0x63, 7),
    (0x64, 7),
    (0x65, 7),
    (0x66, 7),
    (0x67, 7),
    (0x68, 7),
    (0x69, 7),
    (0x6a, 7),
    (0x6b, 7),
    (0x6c, 7),
    (0x6d, 7),
    (0x6e, 7),
    (0x6f, 7),
    (0x70, 7),
    (0x71, 7),
    (0x72, 7),
    (0xfc, 8),
    (0x73, 7),
    (0xfd, 8),
    (0x1ffb, 13),
    (0x7fff0, 19),
    (0x1ffc, 13),
    (0x3ffc, 14),
    (0x22, 6),
    (0x7ffd, 15),
    (0x3, 5),
    (0x23, 6),
    (0x4, 5),
    (0x24, 6),
    (0x5, 5),
    (0x25, 6),
    (0x26, 6),
    (0x27, 6),
    (0x6, 5),
    (0x74, 7),
    (0x75, 7),
    (0x28, 6),
    (0x29, 6),
    (0x2a, 6),
    (0x7, 5),
    (0x2b, 6),
    (0x76, 7),
    (0x2c, 6),
    (0x8, 5),
    (0x9, 5),
    (0x2d, 6),
    (0x77, 7),
    (0x78, 7),
    (0x79, 7),
    (0x7a, 7),
    (0x7b, 7),
    (0x7ffe, 15),
    (0x7fc, 11),
    (0x3ffd, 14),
    (0x1ffd, 13),
    (0xffffffc, 28),
    (0xfffe6, 20),
    (0x3fffd2, 22),
    (0xfffe7, 20),
    (0xfffe8, 20),
    (0x3fffd3, 22),
    (0x3fffd4, 22),
    (0x3fffd5, 22),
    (0x7fffd9, 23),
    (0x3fffd6, 22),
    (0x7fffda, 23),
    (0x7fffdb, 23),
    (0x7fffdc, 23),
    (0x7fffdd, 23),
    (0x7fffde, 23),
    (0xffffeb, 24),
    (0x7fffdf, 23),
    (0xffffec, 24),
    (0xffffed, 24),
    (0x3fffd7, 22),
    (0x7fffe0, 23),
    (0xffffee, 24),
    (0x7fffe1, 23),
    (0x7fffe2, 23),
    (0x7fffe3, 23),
    (0x7fffe4, 23),
    (0x1fffdc, 21),
    (0x3fffd8, 22),
    (0x7fffe5, 23),
    (0x3fffd9, 22),
    (0x7fffe6, 23),
    (0x7fffe7, 23),
    (0xffffef, 24),
    (0x3fffda, 22),
    (0x1fffdd, 21),
    (0xfffe9, 20),
    (0x3fffdb, 22),
    (0x3fffdc, 22),
    (0x7fffe8, 23),
    (0x7fffe9, 23),
    (0x1fffde, 21),
    (0x7fffea, 23),
    (0x3fffdd, 22),
    (0x3fffde, 22),
    (0xfffff0, 24),
    (0x1fffdf, 21),
    (0x3fffdf, 22),
    (0x7fffeb, 23),
    (0x7fffec, 23),
    (0x1fffe0, 21),
    (0x1fffe1, 21),
    (0x3fffe0, 22),
    (0x1fffe2, 21),
    (0x7fffed, 23),
    (0x3fffe1, 22),
    (0x7fffee, 23),
    (0x7fffef, 23),
    (0xfffea, 20),
    (0x3fffe2, 22),
    (0x3fffe3, 22),
    (0x3fffe4, 22),
    (0x7ffff0, 23),
    (0x3fffe5, 22),
    (0x3fffe6, 22),
    (0x7ffff1, 23),
    (0x3ffffe0, 26),
    (0x3ffffe1, 26),
    (0xfffeb, 20),
    (0x7fff1, 19),
    (0x3fffe7, 22),
    (0x7ffff2, 23),
    (0x3fffe8, 22),
    (0x1ffffec, 25),
    (0x3ffffe2, 26),
    (0x3ffffe3, 26),
    (0x3ffffe4, 26),
    (0x7ffffde, 27),
    (0x7ffffdf, 27),
    (0x3ffffe5, 26),
    (0xfffff1, 24),
    (0x1ffffed, 25),
    (0x7fff2, 19),
    (0x1fffe3, 21),
    (0x3ffffe6, 26),
    (0x7ffffe0, 27),
    (0x7ffffe1, 27),
    (0x3ffffe7, 26),
    (0x7ffffe2, 27),
    (0xfffff2, 24),
    (0x1fffe4, 21),
    (0x1fffe5, 21),
    (0x3ffffe8, 26),
    (0x3ffffe9, 26),
    (0xffffffd, 28),
    (0x7ffffe3, 27),
    (0x7ffffe4, 27),
    (0x7ffffe5, 27),
    (0xfffec, 20),
    (0xfffff3, 24),
    (0xfffed, 20),
    (0x1fffe6, 21),
    (0x3fffe9, 22),
    (0x1fffe7, 21),
    (0x1fffe8, 21),
    (0x7ffff3, 23),
    (0x3fffea, 22),
    (0x3fffeb, 22),
    (0x1ffffee, 25),
    (0x1ffffef, 25),
    (0xfffff4, 24),
    (0xfffff5, 24),
    (0x3ffffea, 26),
    (0x7ffff4, 23),
    (0x3ffffeb, 26),
    (0x7ffffe6, 27),
    (0x3ffffec, 26),
    (0x3ffffed, 26),
    (0x7ffffe7, 27),
    (0x7ffffe8, 27),
    (0x7ffffe9, 27),
    (0x7ffffea, 27),
    (0x7ffffeb, 27),
    (0xffffffe, 28),
    (0x7ffffec, 27),
    (0x7ffffed, 27),
    (0x7ffffee, 27),
    (0x7ffffef, 27),
    (0x7fffff0, 27),
    (0x3ffffee, 26),
    (0x3fffffff, 30),
];

#[cfg(test)]
mod tests {
    use super::BitIterator;
    use super::HuffmanDecoder;
    use super::HuffmanDecoderError;

    /// A helper function that converts the given slice containing values `1`
    /// and `0` to a `Vec` of `bool`s, according to the number.
    fn to_expected_bit_result(numbers: &[u8]) -> Vec<bool> {
        numbers.iter().map(|b| -> bool {
            *b == 1
        }).collect()
    }

    #[test]
    fn test_bit_iterator_single_byte() {
        let expected_result = to_expected_bit_result(
            &[0, 0, 0, 0, 1, 0, 1, 0]);

        let mut res: Vec<bool> = Vec::new();
        for b in BitIterator::new(vec![10u8].iter()) {
            res.push(b);
        }

        assert_eq!(res, expected_result);
    }

    #[test]
    fn test_bit_iterator_multiple_bytes() {
        let expected_result = to_expected_bit_result(
            &[0, 0, 0, 0, 1, 0, 1, 0,
              1, 1, 1, 1, 1, 1, 1, 1,
              1, 0, 0, 0, 0, 0, 0, 0,
              0, 0, 0, 0, 0, 0, 0, 1,
              0, 0, 0, 0, 0, 0, 0, 0,
              1, 0, 1, 0, 1, 0, 1, 0]);

        let mut res: Vec<bool> = Vec::new();
        for b in BitIterator::new(vec![10u8, 255, 128, 1, 0, 170].iter()) {
            res.push(b);
        }

        assert_eq!(res, expected_result);
    }

    /// Simple tests for the Huffman decoder -- whether it can decode a single
    /// character code represented additionally with only a single byte.
    #[test]
    fn test_huffman_code_single_byte() {
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
    }

    /// Tests that the Huffman decoder can decode a single character made of
    /// multiple bytes.
    #[test]
    fn test_huffman_code_single_char_multiple_byte() {
        let mut decoder = HuffmanDecoder::new();
        // (The + (2^n - 1) at the final byte is to add the correct expected
        // padding: 1s)
        {
            let hex_buffer = [255, 160 + 15];
            let expected_result = vec![b'#'];

            let result = decoder.decode(&hex_buffer).ok().unwrap();

            assert_eq!(result, expected_result);
        }
        {
            let hex_buffer = [255, 200 + 7];
            let expected_result = vec![b'$'];

            let result = decoder.decode(&hex_buffer).ok().unwrap();

            assert_eq!(result, expected_result);
        }
        {
            let hex_buffer = [255, 255, 255, 240 + 3];
            let expected_result = vec![10];

            let result = decoder.decode(&hex_buffer).ok().unwrap();

            assert_eq!(result, expected_result);
        }
    }

    #[test]
    fn test_huffman_code_multiple_chars() {
        let mut decoder = HuffmanDecoder::new();
        {
            let hex_buffer = [254, 1];
            let expected_result = vec![b'!', b'0'];

            let result = decoder.decode(&hex_buffer).ok().unwrap();

            assert_eq!(result, expected_result);
        }
        {
            let hex_buffer = [(0x14 << 2) | 0x3, 248];
            let expected_result = vec![b' ', b'!'];

            let result = decoder.decode(&hex_buffer).ok().unwrap();

            assert_eq!(result, expected_result);
        }
    }

    /// Tests that if we find the EOS symbol in the stream, we consider it an
    /// error.
    #[test]
    fn test_eos_is_error() {
        let mut decoder = HuffmanDecoder::new();
        {
            // The EOS is an all-ones pattern of length 30
            let hex_buffer = [0xFF, 0xFF, 0xFF, 0xFF];

            let result = decoder.decode(&hex_buffer);

            assert_eq!(HuffmanDecoderError::EOSInString, match result {
                Err(e) => e,
                _ => panic!("Expected error due to EOS symbol in string"),
            });
        }
        {
            // Full EOS after a valid character.
            let hex_buffer = [0x3F, 0xFF, 0xFF, 0xFF, 0xFF];

            let result = decoder.decode(&hex_buffer);

            assert_eq!(HuffmanDecoderError::EOSInString, match result {
                Err(e) => e,
                _ => panic!("Expected error due to EOS symbol in string"),
            });
        }
    }

    /// Tests that when there are 7 or less padding bits, the string is
    /// correctly decoded.
    #[test]
    fn test_short_padding_okay() {
        let mut decoder = HuffmanDecoder::new();
        let hex_buffer = [0x3F];

        let result = decoder.decode(&hex_buffer);

        assert_eq!(b"o".to_vec(), result.ok().unwrap());
    }

    /// Tests that when there are more than 7 padding bits, we get an error.
    #[test]
    fn test_padding_too_long() {
        let mut decoder = HuffmanDecoder::new();
        let hex_buffer = [0x3F, 0xFF];

        let result = decoder.decode(&hex_buffer);

        assert_eq!(HuffmanDecoderError::PaddingTooLarge, match result {
            Err(e) => e,
            _ => panic!("Expected `PaddingTooLarge` error"),
        });
    }

    /// Tests that when there is a certain number of padding bits that deviate
    /// from the most significant bits of the EOS symbol, we get an error.
    #[test]
    fn test_padding_invalid() {
        let mut decoder = HuffmanDecoder::new();
        {
            let hex_buffer = [0x3E];

            let result = decoder.decode(&hex_buffer);

            assert_eq!(HuffmanDecoderError::InvalidPadding, match result {
                Err(e) => e,
                _ => panic!("Expected `InvalidPadding` error"),
            });
        }
        {
            let hex_buffer = [254, 0];

            let result = decoder.decode(&hex_buffer);

            assert_eq!(HuffmanDecoderError::InvalidPadding, match result {
                Err(e) => e,
                _ => panic!("Expected `InvalidPadding` error"),
            });
        }
    }
}
