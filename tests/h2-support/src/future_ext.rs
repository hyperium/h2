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

    /// Panic on success, yielding the content of an `Err`.
    fn unwrap_err(self) -> UnwrapErr<Self>
    where
        Self: Sized,
        Self::Error: fmt::Debug,
    {
        UnwrapErr {
            inner: self,
        }
    }

    /// Panic on success, with a message.
    fn expect_err<T>(self, msg: T) -> ExpectErr<Self>
    where
        Self: Sized,
        Self::Error: fmt::Debug,
        T: fmt::Display,
    {
        ExpectErr{
            inner: self,
            msg: msg.to_string(),
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

    /// Wrap this future in one that will yield NotReady once before continuing.
    ///
    /// This allows the executor to poll other futures before trying this one
    /// again.
    fn yield_once(self) -> Box<Future<Item = Self::Item, Error = Self::Error>>
    where
        Self: Future + Sized + 'static,
    {
        let mut ready = false;
        Box::new(::futures::future::poll_fn(move || {
            if ready {
                Ok::<_, ()>(().into())
            } else {
                ready = true;
                ::futures::task::current().notify();
                Ok(::futures::Async::NotReady)
            }
        }).then(|_| self))
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

// ===== UnwrapErr ======

/// Panic on success.
pub struct UnwrapErr<T> {
    inner: T,
}

impl<T> Future for UnwrapErr<T>
where
    T: Future,
    T::Item: fmt::Debug,
    T::Error: fmt::Debug,
{
    type Item = T::Error;
    type Error = ();

    fn poll(&mut self) -> Poll<T::Error, ()> {
        match self.inner.poll() {
            Ok(Async::Ready(v)) => panic!("Future::unwrap_err() on an Ok value: {:?}", v),
            Ok(Async::NotReady) => Ok(Async::NotReady),
            Err(e) => Ok(Async::Ready(e)),
        }
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

// ===== ExpectErr ======

/// Panic on success
pub struct ExpectErr<T> {
    inner: T,
    msg: String,
}

impl<T> Future for ExpectErr<T>
where
    T: Future,
    T::Item: fmt::Debug,
    T::Error: fmt::Debug,
{
    type Item = T::Error;
    type Error = ();

    fn poll(&mut self) -> Poll<T::Error, ()> {
        match self.inner.poll() {
            Ok(Async::Ready(v)) => panic!("{}: {:?}", self.msg, v),
            Ok(Async::NotReady) => Ok(Async::NotReady),
            Err(e) => Ok(Async::Ready(e)),
        }
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
