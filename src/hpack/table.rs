use hpack::Entry;

use tower::http::{HeaderName, StatusCode, Method, Str};
use std::collections::VecDeque;



/*
pub struct Table {
    entries: VecDeque<HeaderPair>,
    max_size: usize,
}

pub enum Entry {
    Header {
        name: HeaderName,
        value: Str,
    },
    Authority(Str),
    Scheme(Str),
    Path(Str),
    Status(StatusCode),
}
impl Table {
    pub fn new(max_size: usize) -> Table {
        Table {
            entries: VecDeque::new(),
            max_size: max_size,
        }
    }
}

impl Default for Table {
    fn default() -> Table {
        // Default maximum size from the HTTP/2.0 spec.
        Table::new(4_096)
    }
}
*/
