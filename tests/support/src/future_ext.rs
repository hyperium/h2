use futures::{Async, Future, Poll};

use std::fmt;

/// Future extension helpers that are useful for tests
pub trait FutureExt: Future {
    /// Panic on error
    fn unwrap(self) -> Unwrap<Self>
    where
        Self: Sized,
        Self::Error: fmt::Debug,
    {
        Unwrap {
            inner: self,
        }
    }

    /// Panic on error, with a message.
    fn expect<T>(self, msg: T) -> Expect<Self>
    where
        Self: Sized,
        Self::Error: fmt::Debug,
        T: fmt::Display,
    {
        Expect {
            inner: self,
            msg: msg.to_string(),
        }
    }

    /// Drive `other` by polling `self`.
    ///
    /// `self` must not resolve before `other` does.
    fn drive<T>(self, other: T) -> Drive<Self, T>
    where
        T: Future,
        T::Error: fmt::Debug,
        Self: Future<Item = ()> + Sized,
        Self::Error: fmt::Debug,
    {
        Drive {
            driver: Some(self),
            future: other,
        }
    }
}

impl<T: Future> FutureExt for T {}

// ===== Unwrap ======

/// Panic on error
pub struct Unwrap<T> {
    inner: T,
}

impl<T> Future for Unwrap<T>
where
    T: Future,
    T::Item: fmt::Debug,
    T::Error: fmt::Debug,
{
    type Item = T::Item;
    type Error = ();

    fn poll(&mut self) -> Poll<T::Item, ()> {
        Ok(self.inner.poll().unwrap())
    }
}


// ===== Expect ======

/// Panic on error
pub struct Expect<T> {
    inner: T,
    msg: String,
}

impl<T> Future for Expect<T>
where
    T: Future,
    T::Item: fmt::Debug,
    T::Error: fmt::Debug,
{
    type Item = T::Item;
    type Error = ();

    fn poll(&mut self) -> Poll<T::Item, ()> {
        Ok(self.inner.poll().expect(&self.msg))
    }
}

// ===== Drive ======

/// Drive a future to completion while also polling the driver
///
/// This is useful for H2 futures that also require the connection to be polled.
pub struct Drive<T, U> {
    driver: Option<T>,
    future: U,
}

impl<T, U> Future for Drive<T, U>
where
    T: Future<Item = ()>,
    U: Future,
    T::Error: fmt::Debug,
    U::Error: fmt::Debug,
{
    type Item = (T, U::Item);
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let mut looped = false;

        loop {
            match self.future.poll() {
                Ok(Async::Ready(val)) => {
                    // Get the driver
                    let driver = self.driver.take().unwrap();

                    return Ok((driver, val).into());
                },
                Ok(_) => {},
                Err(e) => panic!("unexpected error; {:?}", e),
            }

            match self.driver.as_mut().unwrap().poll() {
                Ok(Async::Ready(_)) => {
                    if looped {
                        // Try polling the future one last time
                        panic!("driver resolved before future")
                    } else {
                        looped = true;
                        continue;
                    }
                },
                Ok(Async::NotReady) => {},
                Err(e) => panic!("unexpected error; {:?}", e),
            }

            return Ok(Async::NotReady);
        }
    }
}
