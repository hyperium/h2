use ConnectionError;
use frame::{self, Frame};
use proto::{ReadySink, StreamMap, StreamTransporter, WindowSize};

use futures::*;

#[derive(Debug)]
pub struct StreamTracker<T> {
    inner: T,
}

impl<T, U> StreamTracker<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>
{
    pub fn new(inner: T) -> StreamTracker<T> {
        StreamTracker { inner }
    }
}

impl<T> StreamTransporter for StreamTracker<T> {
    fn streams(&self) -> &StreamMap {
        unimplemented!()
    }

    fn streams_mut(&mut self) -> &mut StreamMap {
        unimplemented!()
    }
}

impl<T, U> Stream for StreamTracker<T>
    where T: Stream<Item = Frame<U>, Error = ConnectionError>,
{
    type Item = T::Item;
    type Error = T::Error;

    fn poll(&mut self) -> Poll<Option<T::Item>, T::Error> {
        self.inner.poll()
    }
}


impl<T, U> Sink for StreamTracker<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
{
    type SinkItem = T::SinkItem;
    type SinkError = T::SinkError;

    fn start_send(&mut self, item: T::SinkItem) -> StartSend<T::SinkItem, T::SinkError> {
        self.inner.start_send(item)
    }

    fn poll_complete(&mut self) -> Poll<(), T::SinkError> {
        self.inner.poll_complete()
    }
}


impl<T, U> ReadySink for StreamTracker<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ReadySink,
{
    fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        self.inner.poll_ready()
    }
}
