use frame::{self, Frame};

use slab::Slab;

use std::marker::PhantomData;

/// Buffers frames for multiple streams.
#[derive(Debug)]
pub struct Buffer<B> {
    slab: Slab<Slot<B>>,
}

/// A sequence of frames in a `Buffer`
#[derive(Debug)]
pub struct Deque<B> {
    indices: Option<Indices>,
    _p: PhantomData<B>,
}

/// Tracks the head & tail for a sequence of frames in a `Buffer`.
#[derive(Debug, Default, Copy, Clone)]
struct Indices {
    head: usize,
    tail: usize,
}

#[derive(Debug)]
struct Slot<B> {
    frame: Frame<B>,
    next: Option<usize>,
}

impl<B> Buffer<B> {
    pub fn new() -> Self {
        Buffer {
            slab: Slab::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.slab.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slab.is_empty()
    }
}

impl<B> Deque<B> {
    pub fn new() -> Self {
        Deque {
            indices: None,
            _p: PhantomData,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_none()
    }

    pub fn push_back(&mut self, buf: &mut Buffer<B>, frame: Frame<B>) {
        let key = buf.slab.insert(Slot {
            frame,
            next: None,
        });

        match self.indices {
            Some(ref mut idxs) => {
                buf.slab[idxs.tail].next = Some(key);
                idxs.tail = key;
            }
            None => {
                self.indices = Some(Indices {
                    head: key,
                    tail: key,
                });
            }
        }
    }

    pub fn push_front(&mut self, buf: &mut Buffer<B>, frame: Frame<B>) {
        let key = buf.slab.insert(Slot {
            frame,
            next: None,
        });

        match self.indices {
            Some(ref mut idxs) => {
                buf.slab[key].next = Some(idxs.head);
                idxs.head = key;
            }
            None => {
                self.indices = Some(Indices {
                    head: key,
                    tail: key,
                });
            }
        }
    }

    pub fn pop_front(&mut self, buf: &mut Buffer<B>) -> Option<Frame<B>> {
        match self.indices {
            Some(mut idxs) => {
                let mut slot = buf.slab.remove(idxs.head);

                if idxs.head == idxs.tail {
                    assert!(slot.next.is_none());
                    self.indices = None;
                } else {
                    idxs.head = slot.next.take().unwrap();
                    self.indices = Some(idxs);
                }

                return Some(slot.frame);
            }
            None => None,
        }
    }

    pub fn take_while<F>(&mut self, buf: &mut Buffer<B>, mut f: F) -> Self
        where F: FnMut(&Frame<B>) -> bool
    {
        match self.indices {
            Some(mut idxs) => {
                if !f(&buf.slab[idxs.head].frame) {
                    return Deque::new();
                }

                let head = idxs.head;
                let mut tail = idxs.head;

                loop {
                    let next = match buf.slab[tail].next {
                        Some(next) => next,
                        None => {
                            self.indices = None;
                            return Deque {
                                indices: Some(idxs),
                                _p: PhantomData,
                            };
                        }
                    };

                    if !f(&buf.slab[next].frame) {
                        // Split the linked list
                        buf.slab[tail].next = None;

                        self.indices = Some(Indices {
                            head: next,
                            tail: idxs.tail,
                        });

                        return Deque {
                            indices: Some(Indices {
                                head: head,
                                tail: tail,
                            }),
                            _p: PhantomData,
                        }
                    }

                    tail = next;
                }
            }
            None => Deque::new(),
        }
    }
}
