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

    /// Reads the next frame from the underlying socket, eliding ping requests.
    ///
    /// If a PING is received without the ACK flag, the frame is sent to the remote with
    /// its ACK flag set.
    fn poll(&mut self) -> Poll<Option<Frame>, ConnectionError> {
        loop {
            match self.inner.poll() {
                Ok(Async::Ready(Some(Frame::Ping(ping)))) => {
                    if ping.is_ack() {
                        // If we received an ACK, pass it on (nothing to be done here).
                        return Ok(Async::Ready(Some(Frame::Ping(ping))));
                    }

                    // We received a ping request. Try to it send it immediately.  If we
                    // can't send it, save it to be sent (by Sink::poll_complete).
                    let pong = Ping::pong(ping.into_payload());
                    if let AsyncSink::NotReady(pong) = self.start_send(pong.into())? {
                        self.pending_pongs.push_back(pong);
                    }

                    // There's nothing to return yet. Poll the underlying stream again to
                    // determine how to proceed.
                    continue;
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
        // If there are pongs to be sent, try to flush them out before proceeding.
        if !self.pending_pongs.is_empty() {
            self.poll_complete()?;
            if self.pending_pongs.is_empty() {
                /// Couldn't flush pongs, so won't be able to send the item.
                return Ok(AsyncSink::NotReady(item));
            }
        }

        self.inner.start_send(item)
    }

    fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
        // Try to flush the underlying sink.
        let poll = self.inner.poll_complete()?;

        // Then, try to flush pending pongs.
        while let Some(pong) = self.pending_pongs.pop_front() {
            if let AsyncSink::NotReady(pong) = self.inner.start_send(pong)? {
                // If we can't flush all of the pongs, we're not ready. Save the pong to
                // be sent next time.
                self.pending_pongs.push_front(pong);
                return Ok(Async::NotReady);
            }
        }

        // Use the underlying sink's status if pongs are flushed.
        Ok(poll)
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
