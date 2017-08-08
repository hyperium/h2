use super::*;

use slab;

use std::ops;
use std::collections::{HashMap, hash_map};

/// Storage for streams
#[derive(Debug)]
pub(super) struct Store<B> {
    slab: slab::Slab<Stream<B>>,
    ids: HashMap<StreamId, usize>,
}

/// "Pointer" to an entry in the store
pub(super) struct Ptr<'a, B: 'a> {
    key: Key,
    store: &'a mut Store<B>,
}

/// References an entry in the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Key(usize);

pub(super) enum Entry<'a, B: 'a> {
    Occupied(OccupiedEntry<'a, B>),
    Vacant(VacantEntry<'a, B>),
}

pub(super) struct OccupiedEntry<'a, B: 'a> {
    ids: hash_map::OccupiedEntry<'a, StreamId, usize>,
    slab: &'a mut slab::Slab<Stream<B>>,
}

pub(super) struct VacantEntry<'a, B: 'a> {
    ids: hash_map::VacantEntry<'a, StreamId, usize>,
    slab: &'a mut slab::Slab<Stream<B>>,
}

// ===== impl Store =====

impl<B> Store<B> {
    pub fn new() -> Self {
        Store {
            slab: slab::Slab::new(),
            ids: HashMap::new(),
        }
    }

    pub fn resolve(&mut self, key: Key) -> Ptr<B> {
        Ptr {
            key: key,
            store: self,
        }
    }

    pub fn find_mut(&mut self, id: &StreamId) -> Option<&mut Stream<B>> {
        if let Some(handle) = self.ids.get(id) {
            Some(&mut self.slab[*handle])
        } else {
            None
        }
    }

    pub fn insert(&mut self, id: StreamId, val: Stream<B>) -> Ptr<B> {
        let key = self.slab.insert(val);
        assert!(self.ids.insert(id, key).is_none());

        Ptr {
            key: Key(key),
            store: self,
        }
    }

    pub fn find_entry(&mut self, id: StreamId) -> Entry<B> {
        use self::hash_map::Entry::*;

        match self.ids.entry(id) {
            Occupied(e) => {
                Entry::Occupied(OccupiedEntry {
                    ids: e,
                    slab: &mut self.slab,
                })
            }
            Vacant(e) => {
                Entry::Vacant(VacantEntry {
                    ids: e,
                    slab: &mut self.slab,
                })
            }
        }
    }

    pub fn for_each<F>(&mut self, mut f: F)
        where F: FnMut(&mut Stream<B>)
    {
        for &id in self.ids.values() {
            f(&mut self.slab[id])
        }
    }
}

// ===== impl Ptr =====

impl<'a, B: 'a> Ptr<'a, B> {
    pub fn key(&self) -> Key {
        self.key
    }

    pub fn resolve(&mut self, key: Key) -> Ptr<B> {
        Ptr {
            key: key,
            store: self.store,
        }
    }
}

impl<'a, B: 'a> ops::Deref for Ptr<'a, B> {
    type Target = Stream<B>;

    fn deref(&self) -> &Stream<B> {
        &self.store.slab[self.key.0]
    }
}

impl<'a, B: 'a> ops::DerefMut for Ptr<'a, B> {
    fn deref_mut(&mut self) -> &mut Stream<B> {
        &mut self.store.slab[self.key.0]
    }
}

// ===== impl OccupiedEntry =====

impl<'a, B> OccupiedEntry<'a, B> {
    pub fn key(&self) -> Key {
        Key(*self.ids.get())
    }

    pub fn get(&self) -> &Stream<B> {
        &self.slab[*self.ids.get()]
    }

    pub fn get_mut(&mut self) -> &mut Stream<B> {
        &mut self.slab[*self.ids.get()]
    }

    pub fn into_mut(self) -> &'a mut Stream<B> {
        &mut self.slab[*self.ids.get()]
    }
}

// ===== impl VacantEntry =====

impl<'a, B> VacantEntry<'a, B> {
    pub fn insert(self, value: Stream<B>) -> Key {
        // Insert the value in the slab
        let key = self.slab.insert(value);

        // Insert the handle in the ID map
        self.ids.insert(key);

        Key(key)
    }
}
