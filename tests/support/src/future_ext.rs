use futures::{Future, Poll};

use std::fmt;

pub trait FutureExt: Future {
    fn unwrap(self) -> Unwrap<Self>
        where Self: Sized,
              Self::Error: fmt::Debug,
    {
        Unwrap { inner: self }
    }
}

pub struct Unwrap<T> {
    inner: T,
}

impl<T: Future> FutureExt for T {
}

impl<T> Future for Unwrap<T>
    where T: Future,
          T::Item: fmt::Debug,
          T::Error: fmt::Debug,
{
    type Item = T::Item;
    type Error = ();

    fn poll(&mut self) -> Poll<T::Item, ()> {
        Ok(self.inner.poll().unwrap())
    }
}
