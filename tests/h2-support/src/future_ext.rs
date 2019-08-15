use futures::FutureExt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Future extension helpers that are useful for tests
pub trait TestFuture: Future {
    /// Drive `other` by polling `self`.
    ///
    /// `self` must not resolve before `other` does.
    fn drive<T>(&mut self, other: T) -> Drive<'_, Self, T>
    where
        T: Future,
        Self: Future + Sized,
    {
        Drive {
            driver: self,
            future: Box::pin(other),
        }
    }
}

impl<T: Future> TestFuture for T {}

// ===== Drive ======

/// Drive a future to completion while also polling the driver
///
/// This is useful for H2 futures that also require the connection to be polled.
pub struct Drive<'a, T, U> {
    driver: &'a mut T,
    future: Pin<Box<U>>,
}

impl<'a, T, U> Future for Drive<'a, T, U>
where
    T: Future + Unpin,
    U: Future,
{
    type Output = U::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut looped = false;
        loop {
            match self.future.poll_unpin(cx) {
                Poll::Ready(val) => return Poll::Ready(val),
                Poll::Pending => {}
            }

            match self.driver.poll_unpin(cx) {
                Poll::Ready(_) => {
                    if looped {
                        // Try polling the future one last time
                        panic!("driver resolved before future")
                    } else {
                        looped = true;
                        continue;
                    }
                }
                Poll::Pending => {}
            }

            return Poll::Pending;
        }
    }
}
