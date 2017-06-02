use super::Entry;

use fnv::FnvHasher;
use http::method;
use http::header::{self, HeaderName, HeaderValue};

use std::mem;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};

pub struct Table {
    mask: usize,
    indices: Vec<Option<Pos>>,
    slots: VecDeque<Slot>,
    // This tracks the number of evicted elements. It is expected to wrap. This
    // value is used to map `Pos::index` to the actual index in the VecDeque.
    evicted: usize,
    // Size is in bytes
    size: usize,
    max_size: usize,
}

#[derive(Debug)]
pub enum Index<'a> {
    // The entry is already fully indexed
    Indexed(usize),

    // The name is indexed, but not the value
    Name(usize, Entry),

    // The full entry has been inserted into the table.
    Inserted(&'a Entry),

    // The entry is not indexed by this table
    NotIndexed(Entry),
}

struct Slot {
    hash: HashValue,
    entry: Entry,
    next: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
struct Pos {
    index: usize,
    hash: HashValue,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
struct HashValue(usize);

const MAX_SIZE: usize = (1 << 16);

// Index at most this number of values for any given header name.
const MAX_VALUES_PER_NAME: usize = 3;

const DYN_OFFSET: usize = 62;

macro_rules! probe_loop {
    ($probe_var: ident < $len: expr, $body: expr) => {
        debug_assert!($len > 0);
        loop {
            if $probe_var < $len {
                $body
                $probe_var += 1;
            } else {
                $probe_var = 0;
            }
        }
    };
}

impl Table {
    pub fn with_capacity(n: usize) -> Table {
        if n == 0 {
            unimplemented!();
        } else {
            let capacity = to_raw_capacity(n).next_power_of_two();

            Table {
                mask: capacity.wrapping_sub(1),
                indices: vec![None; capacity],
                slots: VecDeque::with_capacity(n),
                evicted: 0,
                size: 0,
                max_size: 4096,
            }
        }
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        usable_capacity(self.indices.len())
    }

    /// Index the entry in the HPACK table.
    pub fn index(&mut self, entry: Entry) -> Index {
        // Check the static table
        let statik = index_static(&entry);

        // Don't index certain headers. This logic is borrowed from nghttp2.
        if entry.skip_value_index() {
            return Index::new(statik, entry);
        }

        // If the header is already indexed by the static table, return that
        if let Some((n, true)) = statik {
            return Index::Indexed(n);
        }

        // Don't index large headers
        if entry.len() * 4 > self.max_size * 3 {
            return Index::new(statik, entry);
        }

        self.index_dynamic(entry, statik)
    }

    fn index_dynamic(&mut self, entry: Entry, statik: Option<(usize, bool)>) -> Index {
        self.reserve_one();

        let hash = hash_entry(&entry);

        let desired_pos = desired_pos(self.mask, hash);
        let mut probe = desired_pos;
        let mut dist = 0;

        // Start at the ideal position, checking all slots
        probe_loop!(probe < self.indices.len(), {
            if let Some(pos) = self.indices[probe] {
                // The slot is already occupied, but check if it has a lower
                // displacement.
                let their_dist = probe_distance(self.mask, pos.hash, probe);

                if their_dist < dist {
                    // Index robinhood
                    return self.index_vacant(entry, hash, desired_pos, probe);
                } else if pos.hash == hash && self.slots[pos.index].entry.key() == entry.key() {
                    // Matching key, check values
                    return self.index_occupied(entry, pos.index, statik);
                }
            } else {
                return self.index_vacant(entry, hash, desired_pos, probe);
            }

            dist += 1;
        });
    }

    fn index_occupied(&mut self, entry: Entry, mut index: usize, statik: Option<(usize, bool)>)
        -> Index
    {
        // There already is a match for the given header name. Check if a value
        // matches. The header will also only be inserted if the table is not at
        // capacity.
        unimplemented!();
    }

    fn index_vacant(&mut self,
                    entry: Entry,
                    hash: HashValue,
                    desired: usize,
                    probe: usize)
        -> Index
    {
        if self.maybe_evict(entry.len()) {
            // Maybe step back
            unimplemented!();
        }

        // The index is offset by the current # of evicted elements
        let slot_idx = self.slots.len();
        let pos_idx = slot_idx.wrapping_add(self.evicted);

        self.slots.push_back(Slot {
            hash: hash,
            entry: entry,
            next: None,
        });

        let mut prev = mem::replace(&mut self.indices[probe], Some(Pos {
            index: pos_idx,
            hash: hash,
        }));

        if let Some(mut prev) = prev {
            // Shift forward
            let mut probe = probe + 1;

            probe_loop!(probe < self.indices.len(), {
                let pos = &mut self.indices[probe as usize];
                let p = mem::replace(pos, Some(prev));

                prev = match mem::replace(pos, Some(prev)) {
                    Some(p) => p,
                    None => break,
                };
            });
        }

        Index::Inserted(&self.slots[slot_idx].entry)
    }

    fn maybe_evict(&mut self, len: usize) -> bool {
        let target = self.max_size - len;
        let mut ret = false;

        while self.size > target {
            ret = true;
            self.evict();
        }

        ret
    }

    fn evict(&mut self) {
        debug_assert!(!self.slots.is_empty());

        // Remove the entry
        let slot = self.slots.pop_front().unwrap();
        let mut probe = desired_pos(self.mask, slot.hash);

        let pos_idx = self.evicted;

        // Find the associated position
        probe_loop!(probe < self.indices.len(), {
            let mut pos = self.indices[probe].unwrap();

            if pos.index == pos_idx {
                if let Some(idx) = slot.next {
                    pos.index = idx;
                    self.indices[probe] = Some(pos);
                } else {
                    self.remove_phase_two(probe);
                }

                break;
            }
        });

        self.evicted = self.evicted.wrapping_add(1);
    }

    // Shifts all indices that were displaced by the entry that has just been
    // removed.
    fn remove_phase_two(&mut self, probe: usize) {
        let mut last_probe = probe;
        let mut probe = probe + 1;

        probe_loop!(probe < self.indices.len(), {
            if let Some(pos) = self.indices[probe] {
                if probe_distance(self.mask, pos.hash, probe) > 0 {
                    self.indices[last_probe] = self.indices[probe].take();
                } else {
                    break;
                }
            } else {
                break;
            }

            last_probe = probe;
        });
    }

    fn reserve_one(&mut self) {
        let len = self.slots.len();

        if len == self.capacity() {
            if len == 0 {
                let new_raw_cap = 8;
                self.mask = 8 - 1;
                self.indices = vec![None; new_raw_cap];
            } else {
                let raw_cap = self.indices.len();
                self.grow(raw_cap << 1);
            }
        }
    }

    #[inline]
    fn grow(&mut self, new_raw_cap: usize) {
        // This path can never be reached when handling the first allocation in
        // the map.

        // find first ideally placed element -- start of cluster
        let mut first_ideal = 0;

        for (i, pos) in self.indices.iter().enumerate() {
            if let Some(pos) = *pos {
                if 0 == probe_distance(self.mask, pos.hash, pos.index) {
                    first_ideal = i;
                    break;
                }
            }
        }

        // visit the entries in an order where we can simply reinsert them
        // into self.indices without any bucket stealing.
        let old_indices = mem::replace(&mut self.indices, vec![None; new_raw_cap]);
        self.mask = new_raw_cap.wrapping_sub(1);

        for &pos in &old_indices[first_ideal..] {
            self.reinsert_entry_in_order(pos);
        }

        for &pos in &old_indices[..first_ideal] {
            self.reinsert_entry_in_order(pos);
        }
    }

    fn reinsert_entry_in_order(&mut self, pos: Option<Pos>) {
        if let Some(pos) = pos {
            // Find first empty bucket and insert there
            let mut probe = desired_pos(self.mask, pos.hash);

            probe_loop!(probe < self.indices.len(), {
                if self.indices[probe as usize].is_none() {
                    // empty bucket, insert here
                    self.indices[probe as usize] = Some(pos);
                    return;
                }
            });
        }
    }
}

impl<'a> Index<'a> {
    fn new(v: Option<(usize, bool)>, e: Entry) -> Index<'a> {
        match v {
            None => Index::NotIndexed(e),
            Some((n, true)) => Index::Indexed(n),
            Some((n, false)) => Index::Name(n, e),
        }
    }
}

#[inline]
fn usable_capacity(cap: usize) -> usize {
    cap - cap / 4
}

#[inline]
fn to_raw_capacity(n: usize) -> usize {
    n + n / 3
}

#[inline]
fn desired_pos(mask: usize, hash: HashValue) -> usize {
    (hash.0 & mask) as usize
}

#[inline]
fn probe_distance(mask: usize, hash: HashValue, current: usize) -> usize {
    current.wrapping_sub(desired_pos(mask, hash)) & mask as usize
}

fn hash_entry(entry: &Entry) -> HashValue {
    const MASK: u64 = (MAX_SIZE as u64) - 1;

    let mut h = FnvHasher::default();
    entry.key().hash(&mut h);
    HashValue((h.finish() & MASK) as usize)
}

/// Checks the static table for the entry. If found, returns the index and a
/// boolean representing if the value matched as well.
fn index_static(entry: &Entry) -> Option<(usize, bool)> {
    match *entry {
        Entry::Header { ref name, ref value } => {
            match *name {
                header::ACCEPT_CHARSET => Some((15, false)),
                header::ACCEPT_ENCODING => {
                    if value == "gzip, deflate" {
                        Some((16, true))
                    } else {
                        Some((16, false))
                    }
                }
                header::ACCEPT_LANGUAGE => Some((17, false)),
                header::ACCEPT_RANGES => Some((18, false)),
                header::ACCEPT => Some((19, false)),
                header::ACCESS_CONTROL_ALLOW_ORIGIN => Some((20, false)),
                header::AGE => Some((21, false)),
                header::ALLOW => Some((22, false)),
                header::AUTHORIZATION => Some((23, false)),
                header::CACHE_CONTROL => Some((24, false)),
                header::CONTENT_DISPOSITION => Some((25, false)),
                header::CONTENT_ENCODING => Some((26, false)),
                header::CONTENT_LANGUAGE => Some((27, false)),
                header::CONTENT_LENGTH => Some((28, false)),
                header::CONTENT_LOCATION => Some((29, false)),
                header::CONTENT_RANGE => Some((30, false)),
                header::CONTENT_TYPE => Some((31, false)),
                header::COOKIE => Some((32, false)),
                header::DATE => Some((33, false)),
                header::ETAG => Some((34, false)),
                header::EXPECT => Some((35, false)),
                header::EXPIRES => Some((36, false)),
                header::FROM => Some((37, false)),
                header::HOST => Some((38, false)),
                header::IF_MATCH => Some((39, false)),
                header::IF_MODIFIED_SINCE => Some((40, false)),
                header::IF_NONE_MATCH => Some((41, false)),
                header::IF_RANGE => Some((42, false)),
                header::IF_UNMODIFIED_SINCE => Some((43, false)),
                header::LAST_MODIFIED => Some((44, false)),
                header::LINK => Some((45, false)),
                header::LOCATION => Some((46, false)),
                header::MAX_FORWARDS => Some((47, false)),
                header::PROXY_AUTHENTICATE => Some((48, false)),
                header::PROXY_AUTHORIZATION => Some((49, false)),
                header::RANGE => Some((50, false)),
                header::REFERER => Some((51, false)),
                header::REFRESH => Some((52, false)),
                header::RETRY_AFTER => Some((53, false)),
                header::SERVER => Some((54, false)),
                header::SET_COOKIE => Some((55, false)),
                header::STRICT_TRANSPORT_SECURITY => Some((56, false)),
                header::TRANSFER_ENCODING => Some((57, false)),
                header::USER_AGENT => Some((58, false)),
                header::VARY => Some((59, false)),
                header::VIA => Some((60, false)),
                header::WWW_AUTHENTICATE => Some((61, false)),
                _ => None,
            }
        }
        Entry::Authority(ref v) => Some((1, false)),
        Entry::Method(ref v) => {
            match *v {
                method::GET => Some((2, true)),
                method::POST => Some((3, true)),
                _ => Some((2, false)),
            }
        }
        Entry::Scheme(ref v) => {
            match &**v {
                "http" => Some((6, true)),
                "https" => Some((7, true)),
                _ => Some((6, false)),
            }
        }
        Entry::Path(ref v) => {
            match &**v {
                "/" => Some((4, true)),
                "/index.html" => Some((5, true)),
                _ => Some((4, false)),
            }
        }
        Entry::Status(ref v) => {
            match u16::from(*v) {
                200 => Some((8, true)),
                204 => Some((9, true)),
                206 => Some((10, true)),
                304 => Some((11, true)),
                400 => Some((12, true)),
                404 => Some((13, true)),
                500 => Some((14, true)),
                _ => Some((8, false)),
            }
        }
    }
}
