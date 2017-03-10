#![allow(warnings)]

extern crate futures;
#[macro_use]
extern crate tokio_io;
extern crate tokio_timer;
extern crate bytes;

pub mod error;
pub mod proto;
pub mod frame;

pub use error::{ConnectionError, StreamError, Reason};

use tokio_io::{AsyncRead, AsyncWrite};
use tokio_io::codec::length_delimited;

use futures::*;

pub struct Transport<T> {
    inner: length_delimited::FramedRead<T>,
}

impl<T: AsyncRead + AsyncWrite> Transport<T> {
    pub fn bind(io: T) -> Transport<T> {
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
}

impl<T: AsyncRead + AsyncWrite> Stream for Transport<T> {
    type Item = frame::Frame;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Option<frame::Frame>, ConnectionError> {
        unimplemented!();
    }
}

impl<T: AsyncRead + AsyncWrite> Sink for Transport<T> {
    type SinkItem = frame::Frame;
    type SinkError = ConnectionError;

    fn start_send(&mut self, item: frame::Frame) -> StartSend<frame::Frame, ConnectionError> {
        unimplemented!();
    }

    fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
        unimplemented!();
    }
}
