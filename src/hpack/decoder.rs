use super::Entry;

use tower::http::{HeaderName, Str};
use bytes::{Buf, Bytes};

use std::io::Cursor;
use std::collections::VecDeque;

/// Decodes headers using HPACK
pub struct Decoder {
    // Protocol indicated that the max table size will update
    max_size_update: Option<usize>,
    table: Table,
}

/// Represents all errors that can be encountered while performing the decoding
/// of an HPACK header set.
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum DecoderError {
    InvalidRepresentation,
    InvalidIntegerPrefix,
    IntegerUnderflow,
    IntegerOverflow,
}

enum Representation {
    /// Indexed header field representation
    ///
    /// An indexed header field representation identifies an entry in either the
    /// static table or the dynamic table (see Section 2.3).
    ///
    /// # Header encoding
    ///
    /// ```text
    ///   0   1   2   3   4   5   6   7
    /// +---+---+---+---+---+---+---+---+
    /// | 1 |        Index (7+)         |
    /// +---+---------------------------+
    /// ```
    Indexed,

    /// Literal Header Field with Incremental Indexing
    ///
    /// A literal header field with incremental indexing representation results
    /// in appending a header field to the decoded header list and inserting it
    /// as a new entry into the dynamic table.
    ///
    /// # Header encoding
    ///
    /// ```text
    ///   0   1   2   3   4   5   6   7
    /// +---+---+---+---+---+---+---+---+
    /// | 0 | 1 |      Index (6+)       |
    /// +---+---+-----------------------+
    /// | H |     Value Length (7+)     |
    /// +---+---------------------------+
    /// | Value String (Length octets)  |
    /// +-------------------------------+
    /// ```
    LiteralWithIndexing,

    /// Literal Header Field without Indexing
    ///
    /// A literal header field without indexing representation results in
    /// appending a header field to the decoded header list without altering the
    /// dynamic table.
    ///
    /// # Header encoding
    ///
    /// ```text
    ///   0   1   2   3   4   5   6   7
    /// +---+---+---+---+---+---+---+---+
    /// | 0 | 0 | 0 | 0 |  Index (4+)   |
    /// +---+---+-----------------------+
    /// | H |     Value Length (7+)     |
    /// +---+---------------------------+
    /// | Value String (Length octets)  |
    /// +-------------------------------+
    /// ```
    LiteralWithoutIndexing,

    /// Literal Header Field Never Indexed
    ///
    /// A literal header field never-indexed representation results in appending
    /// a header field to the decoded header list without altering the dynamic
    /// table. Intermediaries MUST use the same representation for encoding this
    /// header field.
    ///
    /// ```text
    ///   0   1   2   3   4   5   6   7
    /// +---+---+---+---+---+---+---+---+
    /// | 0 | 0 | 0 | 1 |  Index (4+)   |
    /// +---+---+-----------------------+
    /// | H |     Value Length (7+)     |
    /// +---+---------------------------+
    /// | Value String (Length octets)  |
    /// +-------------------------------+
    /// ```
    LiteralNeverIndexed,

    /// Dynamic Table Size Update
    ///
    /// A dynamic table size update signals a change to the size of the dynamic
    /// table.
    ///
    /// # Header encoding
    ///
    /// ```text
    ///   0   1   2   3   4   5   6   7
    /// +---+---+---+---+---+---+---+---+
    /// | 0 | 0 | 1 |   Max size (5+)   |
    /// +---+---------------------------+
    /// ```
    SizeUpdate,
}

struct Table {
    entries: VecDeque<Entry>,
    size: usize,
    max_size: usize,
}

// ===== impl Decoder =====

impl Decoder {
    /// Creates a new `Decoder` with all settings set to default values.
    pub fn new() -> Decoder {
        Decoder {
            max_size_update: None,
            table: Table::new(4_096),
        }
    }

    /// Decodes the headers found in the given buffer.
    pub fn decode<F>(&mut self, src: &Bytes, mut f: F) -> Result<(), DecoderError>
        where F: FnMut(HeaderName, Str)
    {
        use self::Representation::*;

        let mut buf = Cursor::new(src);

        while buf.has_remaining() {
            // At this point we are always at the beginning of the next block
            // within the HPACK data. The type of the block can always be
            // determined from the first byte.
            match try!(Representation::load(peek_u8(&mut buf))) {
                Indexed => {
                    unimplemented!();
                }
                LiteralWithIndexing => {
                    unimplemented!();
                }
                LiteralWithoutIndexing => {
                    unimplemented!();
                }
                LiteralNeverIndexed => {
                    unimplemented!();
                }
                SizeUpdate => {
                    unimplemented!();
                }
            }
        }

        Ok(())
    }

    fn decode_indexed(&self, buf: &mut Cursor<&Bytes>) -> Result<Entry, DecoderError> {
        let index = try!(decode_int(buf, 7));
        self.get_from_table(index)
    }

    fn get_from_table(&self, index: usize) -> Result<Entry, DecoderError> {
        unimplemented!();
        // self.header_table.get(index).o
    }
}

// ===== impl Representation =====

impl Representation {
    pub fn load(byte: u8) -> Result<Representation, DecoderError> {
        const INDEXED: u8                  = 0b10000000;
        const LITERAL_WITH_INDEXING: u8    = 0b01000000;
        const LITERAL_WITHOUT_INDEXING: u8 = 0b11110000;
        const LITERAL_NEVER_INDEXED: u8    = 0b00010000;
        const SIZE_UPDATE: u8              = 0b00100000;

        if byte & INDEXED == INDEXED {
            Ok(Representation::Indexed)
        } else if byte & LITERAL_WITH_INDEXING == LITERAL_WITH_INDEXING {
            Ok(Representation::LiteralWithIndexing)
        } else if byte & LITERAL_WITHOUT_INDEXING == 0 {
            Ok(Representation::LiteralWithIndexing)
        } else if byte & LITERAL_NEVER_INDEXED == LITERAL_NEVER_INDEXED {
            Ok(Representation::LiteralNeverIndexed)
        } else if byte & SIZE_UPDATE == SIZE_UPDATE {
            Ok(Representation::SizeUpdate)
        } else {
            Err(DecoderError::InvalidRepresentation)
        }
    }
}

fn decode_int<B: Buf>(buf: &mut B, prefix_size: u8) -> Result<usize, DecoderError> {
    // The octet limit is chosen such that the maximum allowed *value* can
    // never overflow an unsigned 32-bit integer. The maximum value of any
    // integer that can be encoded with 5 octets is ~2^28
    const MAX_BYTES: usize = 5;
    const VARINT_MASK: u8 = 0b01111111;
    const VARINT_FLAG: u8 = 0b10000000;

    if prefix_size < 1 || prefix_size > 8 {
        return Err(DecoderError::InvalidIntegerPrefix);
    }

    if !buf.has_remaining() {
        return Err(DecoderError::IntegerUnderflow);
    }

    let mask = if prefix_size == 8 {
        0xFF
    } else {
        (1u8 << prefix_size).wrapping_sub(1)
    };

    let mut ret = (buf.get_u8() & mask) as usize;

    if ret < mask as usize {
        // Value fits in the prefix bits
        return Ok(ret);
    }

    // The int did not fit in the prefix bits, so continue reading.
    //
    // The total number of bytes used to represent the int. The first byte was
    // the prefix, so start at 1.
    let mut bytes = 1;

    // The rest of the int is stored as a varint -- 7 bits for the value and 1
    // bit to indicate if it is the last byte.
    let mut shift = 0;

    while buf.has_remaining() {
        let b = buf.get_u8();

        bytes += 1;
        ret += ((b & VARINT_MASK) as usize) << shift;
        shift += 7;

        if b & VARINT_FLAG == VARINT_FLAG {
            return Ok(ret);
        }

        if bytes == MAX_BYTES {
            // The spec requires that this situation is an error
            return Err(DecoderError::IntegerOverflow);
        }
    }

    Err(DecoderError::IntegerUnderflow)
}

fn peek_u8<B: Buf>(buf: &mut B) -> u8 {
    buf.bytes()[0]
}

// ===== impl Table =====

impl Table {
    fn new(max_size: usize) -> Table {
        Table {
            entries: VecDeque::new(),
            size: 0,
            max_size: max_size,
        }
    }

    fn max_size(&self) -> usize {
        self.max_size
    }

    fn insert(&mut self, entry: Entry) {
        let len = entry.len();

        debug_assert!(len <= self.max_size);

        while self.size + len > self.max_size {
            let last = self.entries.pop_back()
                .expect("size of table != 0, but no headers left!");

            self.size -= last.len();
        }

        self.size += len;

        // Track the entry
        self.entries.push_front(entry);
    }
}
