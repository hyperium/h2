use ConnectionError;
use frame::Frame;

use futures::*;
use bytes::BytesMut;

use std::io;

pub struct FramedRead<T> {
    inner: T,
}

impl<T> Stream for FramedRead<T>
    where T: Stream<Item = BytesMut, Error = io::Error>,
{
    type Item = Frame;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Option<Frame>, ConnectionError> {
        match try_ready!(self.inner.poll()) {
            Some(bytes) => {
                Frame::load(bytes.freeze())
                    .map(|frame| Async::Ready(Some(frame)))
                    .map_err(ConnectionError::from)
            }
            None => Ok(Async::Ready(None)),
        }
    }
}

impl<T: Sink> Sink for FramedRead<T> {
    type SinkItem = T::SinkItem;
    type SinkError = T::SinkError;

    fn start_send(&mut self, item: T::SinkItem) -> StartSend<T::SinkItem, T::SinkError> {
        self.inner.start_send(item)
    }

    fn poll_complete(&mut self) -> Poll<(), T::SinkError> {
        self.inner.poll_complete()
    }
}

