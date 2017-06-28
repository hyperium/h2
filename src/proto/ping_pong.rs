use ConnectionError;
use frame::{Frame, Ping};
use futures::*;
use proto::ReadySink;
use std::collections::VecDeque;

/// Acknowledges ping requests from the remote.
#[derive(Debug)]
pub struct PingPong<T> {
    inner: T,
    pending_pongs: VecDeque<Frame>,
}

impl<T> PingPong<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame, SinkError = ConnectionError>,
{
    pub fn new(inner: T) -> PingPong<T> {
        PingPong {
            inner,
            pending_pongs: VecDeque::new(),
        }
    }
}

/// > Receivers of a PING frame that does not include an ACK flag MUST send
/// > a PING frame with the ACK flag set in response, with an identical
/// > payload. PING responses SHOULD be given higher priority than any
/// > other frame.
impl<T> Stream for PingPong<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame, SinkError = ConnectionError>,
{
    type Item = Frame;
    type Error = ConnectionError;

    /// Reads the next frame from the underlying socket.
    ///
    /// If a PING frame is received without the ACK flag, the frame is returned the remote
    /// with the ACK flag.
    fn poll(&mut self) -> Poll<Option<Frame>, ConnectionError> {
        loop {
            match self.inner.poll() {
                Ok(Async::Ready(Some(Frame::Ping(ping)))) => {
                    if !ping.is_ack() {
                        let pong = Ping::pong(ping.into_payload());
                        if !self.pending_pongs.is_empty() {
                            self.pending_pongs.push_back(pong.into());
                        } else if let AsyncSink::NotReady(pong) = self.start_send(pong.into())? {
                            self.pending_pongs.push_back(pong.into());
                        }
                        continue;
                    }
                    return Ok(Async::Ready(Some(Frame::Ping(ping))));
                }
                poll => return poll,
            }
        }
    }
}

impl<T> Sink for PingPong<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame, SinkError = ConnectionError>,
{
    type SinkItem = Frame;
    type SinkError = ConnectionError;

    fn start_send(&mut self, item: Frame) -> StartSend<Frame, ConnectionError> {
        if !self.pending_pongs.is_empty() && self.poll_complete()?.is_not_ready() {
            return Ok(AsyncSink::NotReady(item));
        }

        self.inner.start_send(item)
    }

    fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
        if self.inner.poll_complete()?.is_not_ready() {
            return Ok(Async::NotReady);
        }

        while let Some(pong) = self.pending_pongs.pop_front() {
            if let AsyncSink::NotReady(pong) = self.inner.start_send(pong)? {
                self.pending_pongs.push_front(pong);
                return Ok(Async::NotReady);
            }
        }

        Ok(Async::Ready(()))
    }
}

impl<T> ReadySink for PingPong<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame, SinkError = ConnectionError>,
          T: ReadySink,
{
    fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        if !self.pending_pongs.is_empty() {
            return Ok(Async::NotReady);
        }
        self.inner.poll_ready()
    }
}
