use super::Entry;

use tower::http::{HeaderName, StatusCode, Method, Str};
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
    InvalidTableIndex,
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
        where F: FnMut(Entry)
    {
        use self::Representation::*;

        let mut buf = Cursor::new(src);

        while buf.has_remaining() {
            // At this point we are always at the beginning of the next block
            // within the HPACK data. The type of the block can always be
            // determined from the first byte.
            match try!(Representation::load(peek_u8(&mut buf))) {
                Indexed => {
                    f(try!(self.decode_indexed(&mut buf)));
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
        self.table.get(index)
    }

    fn decode_literal(&self, buf: &mut Cursor<&Bytes>, index: bool) -> Result<Entry, DecoderError> {
        let prefix = if index {
            6
        } else {
            4
        };

        // Extract the table index for the name, or 0 if not indexed
        let table_idx = try!(decode_int(buf, prefix));

        unimplemented!();
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


    /// Returns the entry located at the given index.
    ///
    /// The table is 1-indexed and constructed in such a way that the first
    /// entries belong to the static table, followed by entries in the dynamic
    /// table. They are merged into a single index address space, though.
    ///
    /// This is according to the [HPACK spec, section 2.3.3.]
    /// (http://http2.github.io/http2-spec/compression.html#index.address.space)
    pub fn get(&self, index: usize) -> Result<Entry, DecoderError> {
        if index == 0 {
            return Err(DecoderError::InvalidTableIndex);
        }

        if index <= 61 {
            return Ok(get_static(index));
        }

        // Convert the index for lookup in the entries structure.
        match self.entries.get(index - 62) {
            Some(e) => Ok(e.clone()),
            None => Err(DecoderError::InvalidTableIndex),
        }
    }

    fn insert(&mut self, entry: Entry) {
        let len = entry.len();

        self.reserve(len);

        self.size += len;

        // Track the entry
        self.entries.push_front(entry);
    }

    fn reserve(&mut self, size: usize) {
        debug_assert!(size <= self.max_size);

        while self.size + size > self.max_size {
            let last = self.entries.pop_back()
                .expect("size of table != 0, but no headers left!");

            self.size -= last.len();
        }
    }
}

/// Get an entry from the static table
pub fn get_static(idx: usize) -> Entry {
    use tower::http::StandardHeader::*;

    match idx {
        1 => Entry::Authority(Str::new()),
        2 => Entry::Method(Method::Get),
        3 => Entry::Method(Method::Post),
        4 => Entry::Path(Str::from_static("/")),
        5 => Entry::Path(Str::from_static("/index.html")),
        6 => Entry::Scheme(Str::from_static("http")),
        7 => Entry::Scheme(Str::from_static("https")),
        8 => Entry::Status(StatusCode::Ok),
        9 => Entry::Status(StatusCode::NoContent),
        10 => Entry::Status(StatusCode::PartialContent),
        11 => Entry::Status(StatusCode::NotModified),
        12 => Entry::Status(StatusCode::BadRequest),
        13 => Entry::Status(StatusCode::NotFound),
        14 => Entry::Status(StatusCode::InternalServerError),
        15 => Entry::Header {
            name: AcceptCharset.into(),
            value: Str::new(),
        },
        16 => Entry::Header {
            name: AcceptEncoding.into(),
            value: Str::from_static("gzip, deflate"),
        },
        17 => Entry::Header {
            name: AcceptLanguage.into(),
            value: Str::new(),
        },
        18 => Entry::Header {
            name: AcceptRanges.into(),
            value: Str::new(),
        },
        19 => Entry::Header {
            name: Accept.into(),
            value: Str::new(),
        },
        20 => Entry::Header {
            name: AccessControlAllowOrigin.into(),
            value: Str::new(),
        },
        21 => Entry::Header {
            name: Age.into(),
            value: Str::new(),
        },
        22 => Entry::Header {
            name: Allow.into(),
            value: Str::new(),
        },
        23 => Entry::Header {
            name: Authorization.into(),
            value: Str::new(),
        },
        24 => Entry::Header {
            name: CacheControl.into(),
            value: Str::new(),
        },
        25 => Entry::Header {
            name: ContentDisposition.into(),
            value: Str::new(),
        },
        26 => Entry::Header {
            name: ContentEncoding.into(),
            value: Str::new(),
        },
        27 => Entry::Header {
            name: ContentLanguage.into(),
            value: Str::new(),
        },
        28 => Entry::Header {
            name: ContentLength.into(),
            value: Str::new(),
        },
        29 => Entry::Header {
            name: ContentLocation.into(),
            value: Str::new(),
        },
        30 => Entry::Header {
            name: ContentRange.into(),
            value: Str::new(),
        },
        31 => Entry::Header {
            name: ContentType.into(),
            value: Str::new(),
        },
        32 => Entry::Header {
            name: Cookie.into(),
            value: Str::new(),
        },
        33 => Entry::Header {
            name: Date.into(),
            value: Str::new(),
        },
        34 => Entry::Header {
            name: Etag.into(),
            value: Str::new(),
        },
        35 => Entry::Header {
            name: Expect.into(),
            value: Str::new(),
        },
        36 => Entry::Header {
            name: Expires.into(),
            value: Str::new(),
        },
        37 => Entry::Header {
            name: From.into(),
            value: Str::new(),
        },
        38 => Entry::Header {
            name: Host.into(),
            value: Str::new(),
        },
        39 => Entry::Header {
            name: IfMatch.into(),
            value: Str::new(),
        },
        40 => Entry::Header {
            name: IfModifiedSince.into(),
            value: Str::new(),
        },
        41 => Entry::Header {
            name: IfNoneMatch.into(),
            value: Str::new(),
        },
        42 => Entry::Header {
            name: IfRange.into(),
            value: Str::new(),
        },
        43 => Entry::Header {
            name: IfUnmodifiedSince.into(),
            value: Str::new(),
        },
        44 => Entry::Header {
            name: LastModified.into(),
            value: Str::new(),
        },
        45 => Entry::Header {
            name: Link.into(),
            value: Str::new(),
        },
        46 => Entry::Header {
            name: Location.into(),
            value: Str::new(),
        },
        47 => Entry::Header {
            name: MaxForwards.into(),
            value: Str::new(),
        },
        48 => Entry::Header {
            name: ProxyAuthenticate.into(),
            value: Str::new(),
        },
        49 => Entry::Header {
            name: ProxyAuthorization.into(),
            value: Str::new(),
        },
        50 => Entry::Header {
            name: Range.into(),
            value: Str::new(),
        },
        51 => Entry::Header {
            name: Referer.into(),
            value: Str::new(),
        },
        52 => Entry::Header {
            name: Refresh.into(),
            value: Str::new(),
        },
        53 => Entry::Header {
            name: RetryAfter.into(),
            value: Str::new(),
        },
        54 => Entry::Header {
            name: Server.into(),
            value: Str::new(),
        },
        55 => Entry::Header {
            name: SetCookie.into(),
            value: Str::new(),
        },
        56 => Entry::Header {
            name: StrictTransportSecurity.into(),
            value: Str::new(),
        },
        57 => Entry::Header {
            name: TransferEncoding.into(),
            value: Str::new(),
        },
        58 => Entry::Header {
            name: UserAgent.into(),
            value: Str::new(),
        },
        59 => Entry::Header {
            name: Vary.into(),
            value: Str::new(),
        },
        60 => Entry::Header {
            name: Via.into(),
            value: Str::new(),
        },
        61 => Entry::Header {
            name: WwwAuthenticate.into(),
            value: Str::new(),
        },
        _ => unreachable!(),
    }
}
