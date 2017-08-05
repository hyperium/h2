//! Utilities to support tests.

pub extern crate bytes;
pub extern crate h2;
pub extern crate http;
pub extern crate futures;
pub extern crate mock_io;
pub extern crate env_logger;

pub use self::futures::{
    Future,
    Sink,
    Stream,
};

pub use self::http::{
    request,
    response,
    method,
    status,
    Request,
    Response,
};

pub use self::h2::client::{self, Client};
// pub use self::h2::server;

pub use self::bytes::{
    Buf,
    BufMut,
    Bytes,
    BytesMut,
};

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

pub mod frames {
    //! Some useful frames

    pub const SETTINGS: &'static [u8] = &[0, 0, 0, 4, 0, 0, 0, 0, 0];
    pub const SETTINGS_ACK: &'static [u8] = &[0, 0, 0, 4, 1, 0, 0, 0, 0];
}

#[macro_export]
macro_rules! assert_user_err {
    ($actual:expr, $err:ident) => {{
        use h2::error::{ConnectionError, User};

        match $actual {
            ConnectionError::User(e) => assert_eq!(e, User::$err),
            _ => panic!("unexpected connection error type"),
        }
    }};
}

#[macro_export]
macro_rules! assert_proto_err {
    ($actual:expr, $err:ident) => {{
        use h2::error::{ConnectionError, Reason};

        match $actual {
            ConnectionError::Proto(e) => assert_eq!(e, Reason::$err),
            _ => panic!("unexpected connection error type"),
        }
    }};
}
