#![allow(warnings)]

extern crate futures;
extern crate tokio_io;
extern crate tokio_timer;
extern crate bytes;

pub mod frame;

use tokio_io::{AsyncRead, AsyncWrite};
use tokio_io::codec::length_delimited;

use futures::*;

pub struct Transport<T> {
    inner: length_delimited::FramedRead<T>,
}

pub fn bind<T: AsyncRead + AsyncRead>(io: T) -> Transport<T> {
    let framed = length_delimited::Builder::new()
        .big_endian()
        .length_field_length(3)
        .length_adjustment(6)
        .num_skip(0) // Don't skip the header
        .new_read(io);

    Transport {
        inner: framed,
    }
}
