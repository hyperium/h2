//! Utilities to support tests.

pub extern crate bytes;
pub extern crate h2;
pub extern crate http;
pub extern crate tokio_io;
pub extern crate futures;
pub extern crate mock_io;
pub extern crate env_logger;

#[macro_use]
pub mod codec;

pub use self::futures::{
    Future,
    Sink,
    Stream,
};
pub use self::futures::future::poll_fn;

pub use self::http::{
    request,
    response,
    method,
    status,
    Request,
    Response,
    HeaderMap,
};

pub use self::h2::*;
pub use self::h2::client::{self, Client};
pub use self::h2::server::{self, Server};

pub type Codec<T> = h2::Codec<T, ::std::io::Cursor<::bytes::Bytes>>;

pub use self::bytes::{
    Buf,
    BufMut,
    Bytes,
    BytesMut,
    IntoBuf,
};

pub use std::time::Duration;

use tokio_io::{AsyncRead, AsyncWrite};

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

pub mod frames {
    //! Some useful frames

    pub const SETTINGS: &'static [u8] = &[0, 0, 0, 4, 0, 0, 0, 0, 0];
    pub const SETTINGS_ACK: &'static [u8] = &[0, 0, 0, 4, 1, 0, 0, 0, 0];
}
