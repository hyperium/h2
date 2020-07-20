// Re-export H2 crate
pub use h2;

pub use h2::client;
pub use h2::frame::StreamId;
pub use h2::server;
pub use h2::*;

// Re-export mock
pub use super::mock::{self, idle_ms};

// Re-export frames helpers
pub use super::frames;

// Re-export utility mod
pub use super::util;

// Re-export some type defines
pub use super::{Codec, SendFrame};

// Re-export macros
pub use super::{
    assert_closed, assert_data, assert_default_settings, assert_headers, assert_ping, poll_err,
    poll_frame, raw_codec,
};

pub use super::assert::assert_frame_eq;

// Re-export useful crates
pub use tokio_test::io as mock_io;
pub use {bytes, tracing, tracing_subscriber, futures, http, tokio::io as tokio_io};

// Re-export primary future types
pub use futures::{Future, Sink, Stream};

// And our Future extensions
pub use super::future_ext::TestFuture;

// Our client_ext helpers
pub use super::client_ext::SendRequestExt;

// Re-export HTTP types
pub use http::{uri, HeaderMap, Method, Request, Response, StatusCode, Version};

pub use bytes::{
    buf::{BufExt, BufMutExt},
    Buf, BufMut, Bytes, BytesMut,
};

pub use tokio::io::{AsyncRead, AsyncWrite};

pub use std::thread;
pub use std::time::Duration;

pub static MAGIC_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

// ===== Everything under here shouldn't be used =====
// TODO: work on deleting this code

use futures::future;
pub use futures::future::poll_fn;
use futures::future::Either::*;
use std::pin::Pin;

pub trait MockH2 {
    fn handshake(&mut self) -> &mut Self;

    fn handshake_read_settings(&mut self, settings: &[u8]) -> &mut Self;
}

impl MockH2 for tokio_test::io::Builder {
    fn handshake(&mut self) -> &mut Self {
        self.handshake_read_settings(frames::SETTINGS)
    }

    fn handshake_read_settings(&mut self, settings: &[u8]) -> &mut Self {
        self.write(MAGIC_PREFACE)
            // Settings frame
            .write(frames::SETTINGS)
            .read(settings)
            .read(frames::SETTINGS_ACK)
    }
}

pub trait ClientExt {
    fn run<'a, F: Future + Unpin + 'a>(
        &'a mut self,
        f: F,
    ) -> Pin<Box<dyn Future<Output = F::Output> + 'a>>;
}

impl<T, B> ClientExt for client::Connection<T, B>
where
    T: AsyncRead + AsyncWrite + Unpin + 'static,
    B: Buf + Unpin + 'static,
{
    fn run<'a, F: Future + Unpin + 'a>(
        &'a mut self,
        f: F,
    ) -> Pin<Box<dyn Future<Output = F::Output> + 'a>> {
        let res = future::select(self, f);
        Box::pin(async {
            match res.await {
                Left((Ok(_), b)) => {
                    // Connection is done...
                    b.await
                }
                Right((v, _)) => return v,
                Left((Err(e), _)) => panic!("err: {:?}", e),
            }
        })
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
