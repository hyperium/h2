
// Re-export H2 crate
pub use super::h2;

pub use self::h2::*;
pub use self::h2::client;
pub use self::h2::frame::StreamId;
pub use self::h2::server;

// Re-export mock
pub use super::mock::{self, HandleFutureExt};

// Re-export frames helpers
pub use super::frames;

// Re-export mock notify
pub use super::notify::MockNotify;

// Re-export utility mod
pub use super::util;

// Re-export some type defines
pub use super::{Codec, SendFrame};

// Re-export useful crates
pub use super::{bytes, env_logger, futures, http, mock_io, tokio_io};

// Re-export primary future types
pub use self::futures::{Future, IntoFuture, Sink, Stream};

// And our Future extensions
pub use super::future_ext::{FutureExt, Unwrap};

// Re-export HTTP types
pub use self::http::{uri, HeaderMap, Method, Request, Response, StatusCode, Version};

pub use self::bytes::{Buf, BufMut, Bytes, BytesMut, IntoBuf};

pub use tokio_io::{AsyncRead, AsyncWrite};

pub use std::thread;
pub use std::time::Duration;

// ===== Everything under here shouldn't be used =====
// TODO: work on deleting this code

pub use futures::future::poll_fn;

pub trait MockH2 {
    fn handshake(&mut self) -> &mut Self;
}

impl MockH2 for mock_io::Builder {
    fn handshake(&mut self) -> &mut Self {
        self.write(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
            // Settings frame
            .write(frames::SETTINGS)
            .read(frames::SETTINGS)
            .read(frames::SETTINGS_ACK)
    }
}

pub trait ClientExt {
    fn run<F: Future>(&mut self, f: F) -> Result<F::Item, F::Error>;
}

impl<T, B> ClientExt for client::Connection<T, B>
where
    T: AsyncRead + AsyncWrite + 'static,
    B: IntoBuf + 'static,
{
    fn run<F: Future>(&mut self, f: F) -> Result<F::Item, F::Error> {
        use futures::future::{self, Future};
        use futures::future::Either::*;

        let res = future::poll_fn(|| self.poll()).select2(f).wait();

        match res {
            Ok(A((_, b))) => {
                // Connection is done...
                b.wait()
            },
            Ok(B((v, _))) => return Ok(v),
            Err(A((e, _))) => panic!("err: {:?}", e),
            Err(B((e, _))) => return Err(e),
        }
    }
}

pub fn build_large_headers() -> Vec<(&'static str, String)> {
    vec![
        ("one", "hello".to_string()),
        ("two", build_large_string('2', 4 * 1024)),
        ("three", "three".to_string()),
        ("four", build_large_string('4', 4 * 1024)),
        ("five", "five".to_string()),
        ("six", build_large_string('6', 4 * 1024)),
        ("seven", "seven".to_string()),
        ("eight", build_large_string('8', 4 * 1024)),
        ("nine", "nine".to_string()),
        ("ten", build_large_string('0', 4 * 1024)),
    ]
}

fn build_large_string(ch: char, len: usize) -> String {
    let mut ret = String::new();

    for _ in 0..len {
        ret.push(ch);
    }

    ret
}
