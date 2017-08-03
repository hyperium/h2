use super::*;

use slab;

use std::collections::{HashMap, hash_map};

/// Storage for streams
#[derive(Debug)]
pub(super) struct Store<B> {
    slab: slab::Slab<Stream<B>>,
    ids: HashMap<StreamId, usize>,
}

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

impl<B> Store<B> {
    pub fn new() -> Self {
        Store {
            slab: slab::Slab::new(),
            ids: HashMap::new(),
        }
    }

    pub fn get_mut(&mut self, id: &StreamId) -> Option<&mut Stream<B>> {
        if let Some(handle) = self.ids.get(id) {
            Some(&mut self.slab[*handle])
        } else {
            None
        }
    }

    pub fn insert(&mut self, id: StreamId, val: Stream<B>) {
        let handle = self.slab.insert(val);
        assert!(self.ids.insert(id, handle).is_none());
    }

    pub fn entry(&mut self, id: StreamId) -> Entry<B> {
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
}

impl<'a, B> OccupiedEntry<'a, B> {
    pub fn into_mut(self) -> &'a mut Stream<B> {
        &mut self.slab[*self.ids.get()]
    }
}

impl<'a, B> VacantEntry<'a, B> {
    pub fn insert(self, value: Stream<B>) -> &'a mut Stream<B> {
        // Insert the value in the slab
        let handle = self.slab.insert(value);

        // Insert the handle in the ID map
        self.ids.insert(handle);

        &mut self.slab[handle]
    }
}
