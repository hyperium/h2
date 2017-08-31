mod error;
mod framed_read;
mod framed_write;

pub use self::error::{SendError, RecvError, UserError};
// TODO: don't make these public
pub use self::framed_read::FramedRead;
pub use self::framed_write::FramedWrite;

use frame::{Frame, Data, Settings};

use futures::*;
use tokio_io::{AsyncRead, AsyncWrite};
use bytes::Buf;

#[derive(Debug)]
pub struct Codec<T, B> {
    inner: FramedRead<FramedWrite<T, B>>,
}

impl<T, B> Codec<T, B> {
    pub fn apply_remote_settings(&mut self, frame: &Settings) {
        self.framed_read().apply_remote_settings(frame);
        self.framed_write().apply_remote_settings(frame);
    }

    /// Takes the data payload value that was fully written to the socket
    pub(crate) fn take_last_data_frame(&mut self) -> Option<Data<B>> {
        self.framed_write().take_last_data_frame()
    }

    pub fn max_send_frame_size(&self) -> usize {
        self.inner.get_ref().max_frame_size()
    }

    fn framed_read(&mut self) -> &mut FramedRead<FramedWrite<T, B>> {
        &mut self.inner
    }

    fn framed_write(&mut self) -> &mut FramedWrite<T, B> {
        self.inner.get_mut()
    }
}

impl<T, B> Codec<T, B>
    where T: AsyncRead + AsyncWrite,
          B: Buf,
{
    pub fn from_framed(inner: FramedRead<FramedWrite<T, B>>) -> Self {
        Codec { inner }
    }
}

impl<T, B> Codec<T, B>
    where T: AsyncWrite,
          B: Buf,
{
    pub fn poll_ready(&mut self) -> Poll<(), SendError> {
        self.inner.poll_ready()
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
        self.inner.start_send(item)
    }

    fn poll_complete(&mut self) -> Poll<(), Self::SinkError> {
        self.inner.poll_complete()
    }
}
