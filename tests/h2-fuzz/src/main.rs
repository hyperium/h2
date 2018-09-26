#[macro_use]
extern crate futures;
extern crate tokio_io;
#[macro_use]
extern crate honggfuzz;
extern crate env_logger;
extern crate h2;
extern crate http;

use futures::prelude::*;
use futures::{executor, future, task};
use http::{Method, Request};
use std::cell::Cell;
use std::io::{self, Read, Write};
use std::sync::Arc;
use tokio_io::{AsyncRead, AsyncWrite};

struct MockIo<'a> {
    input: &'a [u8],
}

impl<'a> MockIo<'a> {
    fn next_byte(&mut self) -> Option<u8> {
        if let Some(&c) = self.input.first() {
            self.input = &self.input[1..];
            Some(c)
        } else {
            None
        }
    }

    fn next_u32(&mut self) -> u32 {
        (self.next_byte().unwrap_or(0) as u32) << 8 | self.next_byte().unwrap_or(0) as u32
    }
}

impl<'a> Read for MockIo<'a> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error> {
        let mut len = self.next_u32() as usize;
        if self.input.is_empty() {
            Ok(0)
        } else if len == 0 {
            task::current().notify();
            Err(io::ErrorKind::WouldBlock.into())
        } else {
            if len > self.input.len() {
                len = self.input.len();
            }

            if len > buf.len() {
                len = buf.len();
            }
            buf[0..len].copy_from_slice(&self.input[0..len]);
            self.input = &self.input[len..];
            Ok(len)
        }
    }
}

impl<'a> AsyncRead for MockIo<'a> {
    unsafe fn prepare_uninitialized_buffer(&self, _buf: &mut [u8]) -> bool {
        false
    }
}

impl<'a> Write for MockIo<'a> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, io::Error> {
        let len = std::cmp::min(self.next_u32() as usize, buf.len());
        if len == 0 {
            if self.input.is_empty() {
                Err(io::ErrorKind::BrokenPipe.into())
            } else {
                task::current().notify();
                Err(io::ErrorKind::WouldBlock.into())
            }
        } else {
            Ok(len)
        }
    }

    fn flush(&mut self) -> Result<(), io::Error> {
        Ok(())
    }
}

impl<'a> AsyncWrite for MockIo<'a> {
    fn shutdown(&mut self) -> Poll<(), io::Error> {
        Ok(Async::Ready(()))
    }
}

struct MockNotify {
    notified: Cell<bool>,
}

unsafe impl Sync for MockNotify {}

impl executor::Notify for MockNotify {
    fn notify(&self, _id: usize) {
        self.notified.set(true);
    }
}

impl MockNotify {
    fn take_notify(&self) -> bool {
        self.notified.replace(false)
    }
}

fn run(script: &[u8]) -> Result<(), h2::Error> {
    let notify = Arc::new(MockNotify {
        notified: Cell::new(false),
    });
    let notify_handle: executor::NotifyHandle = notify.clone().into();
    let io = MockIo { input: script };
    let (mut h2, mut connection) = h2::client::handshake(io).wait()?;
    let mut in_progress = None;
    let future = future::poll_fn(|| {
        if let Async::Ready(()) = connection.poll()? {
            return Ok(Async::Ready(()));
        }
        if in_progress.is_none() {
            try_ready!(h2.poll_ready());
            let request = Request::builder()
                .method(Method::POST)
                .uri("https://example.com/")
                .body(())
                .unwrap();
            let (resp, mut send) = h2.send_request(request, false)?;
            send.send_data(vec![0u8; 32769].into(), true).unwrap();
            drop(send);
            in_progress = Some(resp);
        }
        match in_progress.as_mut().unwrap().poll() {
            r @ Ok(Async::Ready(_)) | r @ Err(_) => {
                eprintln!("{:?}", r);
                in_progress = None;
            }
            Ok(Async::NotReady) => (),
        }
        Ok::<_, h2::Error>(Async::NotReady)
    });
    let mut spawn = executor::spawn(future);
    loop {
        if let Async::Ready(()) = spawn.poll_future_notify(&notify_handle, 0)? {
            return Ok(());
        }
        assert!(notify.take_notify());
    }
}

fn main() {
    env_logger::init();
    loop {
        fuzz!(|data: &[u8]| {
            eprintln!("{:?}", run(data));
        });
    }
}
