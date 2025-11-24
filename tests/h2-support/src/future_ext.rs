use futures::{FutureExt, TryFuture};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

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
            future: other.wakened(),
        }
    }

    fn wakened(self) -> Wakened<Self>
    where
        Self: Sized,
    {
        Wakened {
            future: Box::pin(self),
            woken: Arc::new(AtomicBool::new(true)),
        }
    }
}

/// Wraps futures::future::join to ensure that the futures are only polled if they are woken.
pub fn join<Fut1, Fut2>(
    future1: Fut1,
    future2: Fut2,
) -> futures::future::Join<Wakened<Fut1>, Wakened<Fut2>>
where
    Fut1: Future,
    Fut2: Future,
{
    futures::future::join(future1.wakened(), future2.wakened())
}

/// Wraps futures::future::join3 to ensure that the futures are only polled if they are woken.
pub fn join3<Fut1, Fut2, Fut3>(
    future1: Fut1,
    future2: Fut2,
    future3: Fut3,
) -> futures::future::Join3<Wakened<Fut1>, Wakened<Fut2>, Wakened<Fut3>>
where
    Fut1: Future,
    Fut2: Future,
    Fut3: Future,
{
    futures::future::join3(future1.wakened(), future2.wakened(), future3.wakened())
}

/// Wraps futures::future::join4 to ensure that the futures are only polled if they are woken.
pub fn join4<Fut1, Fut2, Fut3, Fut4>(
    future1: Fut1,
    future2: Fut2,
    future3: Fut3,
    future4: Fut4,
) -> futures::future::Join4<Wakened<Fut1>, Wakened<Fut2>, Wakened<Fut3>, Wakened<Fut4>>
where
    Fut1: Future,
    Fut2: Future,
    Fut3: Future,
    Fut4: Future,
{
    futures::future::join4(
        future1.wakened(),
        future2.wakened(),
        future3.wakened(),
        future4.wakened(),
    )
}

/// Wraps futures::future::try_join to ensure that the futures are only polled if they are woken.
pub fn try_join<Fut1, Fut2>(
    future1: Fut1,
    future2: Fut2,
) -> futures::future::TryJoin<Wakened<Fut1>, Wakened<Fut2>>
where
    Fut1: futures::future::TryFuture + Future,
    Fut2: Future,
    Wakened<Fut1>: futures::future::TryFuture,
    Wakened<Fut2>: futures::future::TryFuture<Error = <Wakened<Fut1> as TryFuture>::Error>,
{
    futures::future::try_join(future1.wakened(), future2.wakened())
}

/// Wraps futures::future::select to ensure that the futures are only polled if they are woken.
pub fn select<A, B>(future1: A, future2: B) -> futures::future::Select<Wakened<A>, Wakened<B>>
where
    A: Future + Unpin,
    B: Future + Unpin,
{
    futures::future::select(future1.wakened(), future2.wakened())
}

/// Wraps futures::future::join_all to ensure that the futures are only polled if they are woken.
pub fn join_all<I>(iter: I) -> futures::future::JoinAll<Wakened<I::Item>>
where
    I: IntoIterator,
    I::Item: Future,
{
    futures::future::join_all(iter.into_iter().map(|f| f.wakened()))
}

/// A future that only polls the inner future if it has been woken (after the initial poll).
pub struct Wakened<T> {
    future: Pin<Box<T>>,
    woken: Arc<AtomicBool>,
}

/// A future that only polls the inner future if it has been woken (after the initial poll).
impl<T> Future for Wakened<T>
where
    T: Future,
{
    type Output = T::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if !this.woken.load(std::sync::atomic::Ordering::SeqCst) {
            return Poll::Pending;
        }
        this.woken.store(false, std::sync::atomic::Ordering::SeqCst);
        let my_waker = IfWokenWaker {
            inner: cx.waker().clone(),
            wakened: this.woken.clone(),
        };
        let my_waker = Arc::new(my_waker).into();
        let mut cx = Context::from_waker(&my_waker);
        this.future.as_mut().poll(&mut cx)
    }
}

impl Wake for IfWokenWaker {
    fn wake(self: Arc<Self>) {
        self.wakened
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.inner.wake_by_ref();
    }
}

struct IfWokenWaker {
    inner: Waker,
    wakened: Arc<AtomicBool>,
}

impl<T: Future> TestFuture for T {}

// ===== Drive ======

/// Drive a future to completion while also polling the driver
///
/// This is useful for H2 futures that also require the connection to be polled.
pub struct Drive<'a, T, U> {
    driver: &'a mut T,
    future: Wakened<U>,
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
