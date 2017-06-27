use ConnectionError;
use frame::Frame;
use proto::ReadySink;

use futures::*;

#[derive(Debug)]
pub struct PingPong<T> {
    inner: T,
}

impl<T> PingPong<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame, SinkError = ConnectionError>,
{
    pub fn new(inner: T) -> PingPong<T> {
        PingPong {
            inner: inner,
        }
    }
}

impl<T> Stream for PingPong<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame, SinkError = ConnectionError>,
{
    type Item = Frame;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Option<Frame>, ConnectionError> {
        // returns from socek to application
        self.inner.poll() // inner frame?
            // match on ping without ack.
            // send pong on inner:
            // self.inner.start_send(Pong) ....
            // what happens when sending pong isn't ready?
    }
}

impl<T> Sink for PingPong<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame, SinkError = ConnectionError>,
{
    type SinkItem = Frame;
    type SinkError = ConnectionError;

    fn start_send(&mut self, item: Frame) -> StartSend<Frame, ConnectionError> {
        // ensure pending pong is complete before sending `item`
        // client sends us frames here
        self.inner.start_send(item)
    }

    fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
        self.inner.poll_complete()
            // if pong hasn't sent, poll it before polling inner
    }
}

impl<T> ReadySink for PingPong<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame, SinkError = ConnectionError>,
          T: ReadySink,
{
    fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        self.inner.poll_ready()
    }
}
