
// Re-export H2 crate
pub use super::h2;

pub use self::h2::*;
pub use self::h2::client::{self, Client};
pub use self::h2::server::{self, Server};

// Re-export mock
pub use super::mock::{self, HandleFutureExt};

// Re-export frames helpers
pub use super::frames;

// Re-export some type defines
pub use super::{Codec, SendFrame};

// Re-export useful crates
pub use super::{
    http,
    bytes,
    tokio_io,
    futures,
    mock_io,
    env_logger,
};

// Re-export primary future types
pub use self::futures::{
    Future,
    Sink,
    Stream,
};

// And our Future extensions
pub use future_ext::{FutureExt, Unwrap};

// Re-export HTTP types
pub use self::http::{
    Request,
    Response,
    Method,
    HeaderMap,
    StatusCode,
};

pub use self::bytes::{
    Buf,
    BufMut,
    Bytes,
    BytesMut,
    IntoBuf,
};

pub use tokio_io::{AsyncRead, AsyncWrite};

pub use std::time::Duration;

// ===== Everything under here shouldn't be used =====
// TODO: work on deleting this code

pub use ::futures::future::poll_fn;

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

impl<T, B> ClientExt for Client<T, B>
    where T: AsyncRead + AsyncWrite + 'static,
          B: IntoBuf + 'static,
{
    fn run<F: Future>(&mut self, f: F) -> Result<F::Item, F::Error> {
        use futures::future::{self, Future};
        use futures::future::Either::*;

        let res = future::poll_fn(|| self.poll())
            .select2(f).wait();

        match res {
            Ok(A((_, b))) => {
                // Connection is done...
                b.wait()
            }
            Ok(B((v, _))) => return Ok(v),
            Err(A((e, _))) => panic!("err: {:?}", e),
            Err(B((e, _))) => return Err(e),
        }
    }
}

