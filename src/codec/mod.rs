mod error;
mod framed_read;
mod framed_write;

pub use self::error::{SendError, RecvError, UserError};

use self::framed_read::FramedRead;
use self::framed_write::FramedWrite;

use frame::{self, Frame, Data};

use futures::*;

use tokio_io::{AsyncRead, AsyncWrite};
use tokio_io::codec::length_delimited;

use bytes::Buf;

use std::io;

#[derive(Debug)]
pub struct Codec<T, B> {
    inner: FramedRead<FramedWrite<T, B>>,
}

impl<T, B> Codec<T, B>
    where T: AsyncRead + AsyncWrite,
          B: Buf,
{
    pub fn new(io: T) -> Self {
        // Wrap with writer
        let framed_write = FramedWrite::new(io);

        // Delimit the frames
        let delimited = length_delimited::Builder::new()
            .big_endian()
            .length_field_length(3)
            .length_adjustment(9)
            .num_skip(0) // Don't skip the header
            // TODO: make this configurable and allow it to be changed during
            // runtime.
            .max_frame_length(frame::DEFAULT_MAX_FRAME_SIZE as usize)
            .new_read(framed_write);

        let inner = FramedRead::new(delimited);

        Codec { inner }
    }
}

impl<T, B> Codec<T, B> {
    /// Returns the max frame size that can be sent to the peer.
    pub fn max_send_frame_size(&self) -> usize {
        self.inner.get_ref().max_frame_size()
    }

    /// Set the peer's max frame size.
    pub fn set_max_send_frame_size(&mut self, val: usize) {
        self.framed_write().set_max_frame_size(val)
    }

    /// Get a reference to the inner stream.
    #[cfg(feature = "unstable")]
    pub fn get_ref(&self) -> &T {
        self.inner.get_ref().get_ref()
    }

    /// Get a mutable reference to the inner stream.
    pub fn get_mut(&mut self) -> &mut T {
        self.inner.get_mut().get_mut()
    }

    /// Takes the data payload value that was fully written to the socket
    pub(crate) fn take_last_data_frame(&mut self) -> Option<Data<B>> {
        self.framed_write().take_last_data_frame()
    }

    fn framed_write(&mut self) -> &mut FramedWrite<T, B> {
        self.inner.get_mut()
    }
}

impl<T, B> Codec<T, B>
    where T: AsyncWrite,
          B: Buf,
{
    /// Returns `Ready` when the codec can buffer a frame
    pub fn poll_ready(&mut self) -> Poll<(), io::Error> {
        self.framed_write().poll_ready()
    }

    /// Buffer a frame.
    ///
    /// `poll_ready` must be called first to ensure that a frame may be
    /// accepted.
    ///
    /// TODO: Rename this to avoid conflicts with Sink::buffer
    pub fn buffer(&mut self, item: Frame<B>) -> Result<(), UserError>
    {
        self.framed_write().buffer(item)
    }

    /// Flush buffered data to the wire
    pub fn flush(&mut self) -> Poll<(), io::Error> {
        self.framed_write().flush()
    }

    /// Shutdown the send half
    pub fn shutdown(&mut self) -> Poll<(), io::Error> {
        self.framed_write().shutdown()
    }
}

impl<T, B> Stream for Codec<T, B>
    where T: AsyncRead,
{
    type Item = Frame;
    type Error = RecvError;

    fn poll(&mut self) -> Poll<Option<Frame>, Self::Error> {
        self.inner.poll()
    }
}

impl<T, B> Sink for Codec<T, B>
    where T: AsyncWrite,
          B: Buf,
{
    type SinkItem = Frame<B>;
    type SinkError = SendError;

    fn start_send(&mut self, item: Self::SinkItem) -> StartSend<Self::SinkItem, Self::SinkError> {
        if !self.poll_ready()?.is_ready() {
            return Ok(AsyncSink::NotReady(item));
        }

        self.buffer(item)?;
        Ok(AsyncSink::Ready)
    }

    fn poll_complete(&mut self) -> Poll<(), Self::SinkError> {
        self.flush()?;
        Ok(Async::Ready(()))
    }

    fn close(&mut self) -> Poll<(), Self::SinkError> {
        self.shutdown()?;
        Ok(Async::Ready(()))
    }
}

// TODO: remove (or improve) this
impl<T> From<T> for Codec<T, ::std::io::Cursor<::bytes::Bytes>>
    where T: AsyncRead + AsyncWrite,
{
    fn from(src: T) -> Self {
        Self::new(src)
    }
}
