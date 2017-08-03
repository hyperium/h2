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
#[derive(Debug, Default)]
struct Indices {
    head: usize,
    tail: usize,
}

#[derive(Debug)]
struct Slot<B> {
    frame: Frame<B>,
    next: usize,
}

impl<B> Buffer<B> {
    pub fn new() -> Self {
        Buffer {
            slab: Slab::new(),
        }
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

    pub fn push_back(&mut self, buf: &mut Buffer<B>, val: Frame<B>) {
        unimplemented!();
    }

    pub fn pop_front(&mut self, buf: &mut Buffer<B>) -> Option<Frame<B>> {
        unimplemented!();
    }
}
