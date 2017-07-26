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
    status,
};

pub use self::h2::{
    client,
    server,
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
