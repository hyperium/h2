use super::*;
use futures::*;

#[derive(Debug)]
pub struct Codec<T, B> {
    inner: FramedRead<FramedWrite<T, B>>,
}

impl<T, B> Codec<T, B> {
    pub fn apply_remote_settings(&mut self, frame: &frame::Settings) {
        self.framed_read().apply_remote_settings(frame);
        self.framed_write().apply_remote_settings(frame);
    }

    /// Takes the data payload value that was fully written to the socket
    pub(crate) fn take_last_data_frame(&mut self) -> Option<frame::Data<B>> {
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
    pub fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        self.inner.poll_ready()
    }

}

impl<T, B> futures::Stream for Codec<T, B>
    where T: AsyncRead,
{
    type Item = Frame;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Option<Frame>, ConnectionError> {
        use futures::Stream;
        self.inner.poll()
    }
}

impl<T, B> Sink for Codec<T, B>
    where T: AsyncWrite,
          B: Buf,
{
    type SinkItem = Frame<B>;
    type SinkError = ConnectionError;

    fn start_send(&mut self, item: Self::SinkItem) -> StartSend<Self::SinkItem, Self::SinkError> {
        self.inner.start_send(item)
    }

    fn poll_complete(&mut self) -> Poll<(), Self::SinkError> {
        self.inner.poll_complete()
    }
}
