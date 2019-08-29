//! A mock type implementing [`Read`] and [`Write`].
//!
//! Copied from https://github.com/carllerche/mock-io.
//!
//! TODO:
//! - Either the mock-io crate should be released or this module should be
//!   removed from h2.
//!
//! # Overview
//!
//! Provides a type that implements [`Read`] + [`Write`] that can be configured
//! to handle an arbitrary sequence of read and write operations. This is useful
//! for writing unit tests for networking services as using an actual network
//! type is fairly non deterministic.
//!
//! # Usage
//!
//! Add the following to your `Cargo.toml`
//!
//! ```toml
//! [dependencies]
//! mock-io = { git = "https://github.com/carllerche/mock-io" }
//! ```
//!
//! Then use it in your project. For example, a test could be written:
//!
//! ```
//! use mock_io::{Builder, Mock};
//! use std::io::{Read, Write};
//!
//! # /*
//! #[test]
//! # */
//! fn test_io() {
//!     let mut mock = Builder::new()
//!         .write(b"ping")
//!         .read(b"pong")
//!         .build();
//!
//!    let n = mock.write(b"ping").unwrap();
//!    assert_eq!(n, 4);
//!
//!    let mut buf = vec![];
//!    mock.read_to_end(&mut buf).unwrap();
//!
//!    assert_eq!(buf, b"pong");
//! }
//! # pub fn main() {
//! # test_io();
//! # }
//! ```
//!
//! Attempting to write data that the mock isn't expected will result in a
//! panic.
//!
//! # Tokio
//!
//! `Mock` also supports tokio by implementing `AsyncRead` and `AsyncWrite`.
//! When using `Mock` in context of a Tokio task, it will automatically switch
//! to "async" behavior (this can also be set explicitly by calling `set_async`
//! on `Builder`).
//!
//! In async mode, calls to read and write are non-blocking and the task using
//! the mock is notified when the readiness state changes.
//!
//! # `io-dump` dump files
//!
//! `Mock` can also be configured from an `io-dump` file. By doing this, the
//! mock value will replay a previously recorded behavior. This is useful for
//! collecting a scenario from the real world and replying it as part of a test.
//!
//! [`Read`]: https://doc.rust-lang.org/std/io/trait.Read.html
//! [`Write`]: https://doc.rust-lang.org/std/io/trait.Write.html

#![allow(deprecated)]

use std::collections::VecDeque;
use std::time::{Duration, Instant};
use std::{cmp, io};

/// An I/O handle that follows a predefined script.
///
/// This value is created by `Builder` and implements `Read + `Write`. It
/// follows the scenario described by the builder and panics otherwise.
#[derive(Debug)]
pub struct Mock {
    inner: Inner,
    tokio: tokio_::Inner,
}

#[derive(Debug)]
pub struct Handle {
    inner: tokio_::Handle,
}

/// Builds `Mock` instances.
#[derive(Debug, Clone, Default)]
pub struct Builder {
    // Sequence of actions for the Mock to take
    actions: VecDeque<Action>,
}

#[derive(Debug, Clone)]
enum Action {
    Read(Vec<u8>),
    Write(Vec<u8>),
    Wait(Duration),
}

#[derive(Debug)]
struct Inner {
    actions: VecDeque<Action>,
    waiting: Option<Instant>,
}

impl Builder {
    /// Return a new, empty `Builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sequence a `read` operation.
    ///
    /// The next operation in the mock's script will be to expect a `read` call
    /// and return `buf`.
    pub fn read(&mut self, buf: &[u8]) -> &mut Self {
        self.actions.push_back(Action::Read(buf.into()));
        self
    }

    /// Sequence a `write` operation.
    ///
    /// The next operation in the mock's script will be to expect a `write`
    /// call.
    pub fn write(&mut self, buf: &[u8]) -> &mut Self {
        self.actions.push_back(Action::Write(buf.into()));
        self
    }

    /// Sequence a wait.
    ///
    /// The next operation in the mock's script will be to wait without doing so
    /// for `duration` amount of time.
    pub fn wait(&mut self, duration: Duration) -> &mut Self {
        let duration = cmp::max(duration, Duration::from_millis(1));
        self.actions.push_back(Action::Wait(duration));
        self
    }

    /// Build a `Mock` value according to the defined script.
    pub fn build(&mut self) -> Mock {
        let (mock, _) = self.build_with_handle();
        mock
    }

    /// Build a `Mock` value paired with a handle
    pub fn build_with_handle(&mut self) -> (Mock, Handle) {
        let (tokio, handle) = tokio_::Inner::new();

        let src = self.clone();

        let mock = Mock {
            inner: Inner {
                actions: src.actions,
                waiting: None,
            },
            tokio: tokio,
        };

        let handle = Handle { inner: handle };

        (mock, handle)
    }
}

impl Handle {
    /// Sequence a `read` operation.
    ///
    /// The next operation in the mock's script will be to expect a `read` call
    /// and return `buf`.
    pub fn read(&mut self, buf: &[u8]) -> &mut Self {
        self.inner.read(buf);
        self
    }

    /// Sequence a `write` operation.
    ///
    /// The next operation in the mock's script will be to expect a `write`
    /// call.
    pub fn write(&mut self, buf: &[u8]) -> &mut Self {
        self.inner.write(buf);
        self
    }
}

impl Inner {
    fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        match self.action() {
            Some(&mut Action::Read(ref mut data)) => {
                // Figure out how much to copy
                let n = cmp::min(dst.len(), data.len());

                // Copy the data into the `dst` slice
                (&mut dst[..n]).copy_from_slice(&data[..n]);

                // Drain the data from the source
                data.drain(..n);

                // Return the number of bytes read
                Ok(n)
            }
            Some(_) => {
                // Either waiting or expecting a write
                Err(io::ErrorKind::WouldBlock.into())
            }
            None => Ok(0),
        }
    }

    fn write(&mut self, mut src: &[u8]) -> io::Result<usize> {
        let mut ret = 0;

        if self.actions.is_empty() {
            return Err(io::ErrorKind::BrokenPipe.into());
        }

        match self.action() {
            Some(&mut Action::Wait(..)) => {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            _ => {}
        }

        for i in 0..self.actions.len() {
            match self.actions[i] {
                Action::Write(ref mut expect) => {
                    let n = cmp::min(src.len(), expect.len());

                    assert_eq!(&src[..n], &expect[..n]);

                    // Drop data that was matched
                    expect.drain(..n);
                    src = &src[n..];

                    ret += n;

                    if src.is_empty() {
                        return Ok(ret);
                    }
                }
                Action::Wait(..) => {
                    break;
                }
                _ => {}
            }

            // TODO: remove write
        }

        Ok(ret)
    }

    fn remaining_wait(&mut self) -> Option<Duration> {
        match self.action() {
            Some(&mut Action::Wait(dur)) => Some(dur),
            _ => None,
        }
    }

    fn action(&mut self) -> Option<&mut Action> {
        loop {
            if self.actions.is_empty() {
                return None;
            }

            match self.actions[0] {
                Action::Read(ref mut data) => {
                    if !data.is_empty() {
                        break;
                    }
                }
                Action::Write(ref mut data) => {
                    if !data.is_empty() {
                        break;
                    }
                }
                Action::Wait(ref mut dur) => {
                    if let Some(until) = self.waiting {
                        let now = Instant::now();

                        if now < until {
                            break;
                        }
                    } else {
                        self.waiting = Some(Instant::now() + *dur);
                        break;
                    }
                }
            }

            let _action = self.actions.pop_front();
        }

        self.actions.front_mut()
    }
}

// use tokio::*;

mod tokio_ {
    use super::*;

    use futures::channel::mpsc;
    use futures::{ready, FutureExt, Stream};
    use std::task::{Context, Poll, Waker};
    use tokio::io::{AsyncRead, AsyncWrite};
    use tokio::timer::Delay;

    use std::pin::Pin;

    use std::io;

    #[derive(Debug)]
    pub struct Inner {
        sleep: Option<Delay>,
        read_wait: Option<Waker>,
        rx: mpsc::UnboundedReceiver<Action>,
    }

    #[derive(Debug)]
    pub struct Handle {
        tx: mpsc::UnboundedSender<Action>,
    }

    // ===== impl Handle =====

    impl Handle {
        pub fn read(&mut self, buf: &[u8]) {
            self.tx.unbounded_send(Action::Read(buf.into())).unwrap();
        }

        pub fn write(&mut self, buf: &[u8]) {
            self.tx.unbounded_send(Action::Write(buf.into())).unwrap();
        }
    }

    // ===== impl Inner =====

    impl Inner {
        pub fn new() -> (Inner, Handle) {
            let (tx, rx) = mpsc::unbounded();

            let inner = Inner {
                sleep: None,
                read_wait: None,
                rx: rx,
            };

            let handle = Handle { tx };

            (inner, handle)
        }

        pub(super) fn poll_action(&mut self, cx: &mut Context) -> Poll<Option<Action>> {
            Pin::new(&mut self.rx).poll_next(cx)
        }
    }

    impl Mock {
        fn maybe_wakeup_reader(&mut self) {
            match self.inner.action() {
                Some(&mut Action::Read(_)) | None => {
                    if let Some(task) = self.tokio.read_wait.take() {
                        task.wake();
                    }
                }
                _ => {}
            }
        }
    }

    impl AsyncRead for Mock {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<io::Result<usize>> {
            loop {
                if let Some(sleep) = &mut self.tokio.sleep {
                    ready!(sleep.poll_unpin(cx));
                }

                // If a sleep is set, it has already fired
                self.tokio.sleep = None;

                match self.inner.read(buf) {
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                        if let Some(rem) = self.inner.remaining_wait() {
                            self.tokio.sleep = Some(tokio::timer::delay(Instant::now() + rem));
                        } else {
                            self.tokio.read_wait = Some(cx.waker().clone());
                            return Poll::Pending;
                        }
                    }
                    Ok(0) => {
                        // TODO: Extract
                        match self.tokio.poll_action(cx) {
                            Poll::Ready(Some(action)) => {
                                self.inner.actions.push_back(action);
                                continue;
                            }
                            Poll::Ready(None) => {
                                return Poll::Ready(Ok(0));
                            }
                            Poll::Pending => {
                                return Poll::Pending;
                            }
                        }
                    }
                    ret => return Poll::Ready(ret),
                }
            }
        }
    }

    impl AsyncWrite for Mock {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<Result<usize, io::Error>> {
            loop {
                if let Some(sleep) = &mut self.tokio.sleep {
                    ready!(sleep.poll_unpin(cx));
                }

                // If a sleep is set, it has already fired
                self.tokio.sleep = None;

                match self.inner.write(buf) {
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                        if let Some(rem) = self.inner.remaining_wait() {
                            self.tokio.sleep = Some(tokio::timer::delay(Instant::now() + rem));
                        } else {
                            panic!("unexpected WouldBlock");
                        }
                    }
                    Ok(0) => {
                        // TODO: Is this correct?
                        if !self.inner.actions.is_empty() {
                            return Poll::Pending;
                        }

                        // TODO: Extract
                        match self.tokio.poll_action(cx) {
                            Poll::Ready(Some(action)) => {
                                self.inner.actions.push_back(action);
                                continue;
                            }
                            Poll::Ready(None) => {
                                panic!("unexpected write");
                            }
                            Poll::Pending => return Poll::Pending,
                        }
                    }
                    ret => {
                        self.maybe_wakeup_reader();
                        return Poll::Ready(ret);
                    }
                }
            }
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), io::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    /*
    TODO: Is this required?

    /// Returns `true` if called from the context of a futures-rs Task
    pub fn is_task_ctx() -> bool {
        use std::panic;

        // Save the existing panic hook
        let h = panic::take_hook();

        // Install a new one that does nothing
        panic::set_hook(Box::new(|_| {}));

        // Attempt to call the fn
        let r = panic::catch_unwind(|| task::current()).is_ok();

        // Re-install the old one
        panic::set_hook(h);

        // Return the result
        r
    }
    */
}
