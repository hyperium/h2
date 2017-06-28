use bytes::{Bytes, BufMut};
use ConnectionError;
use frame::{Frame, Ping};
use futures::*;
use proto::ReadySink;
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tokio_timer::{Sleep, Timer};

#[derive(Debug)]
pub struct KeepAlive<T> {
    inner: T,
    timer: Timer,
    ttl: Duration,
    state: Option<State>,
}

#[derive(Debug)]
enum State {
    Idle { last: Instant, should_ping: Sleep },
    /// Could not complete sending ping. It should be sent ASAP.
    Pending { deadline: Sleep, ping: Frame },
    /// A ping has been sent and we expect a response by `deadline`.
    Waiting { deadline: Sleep, remaining: usize },
}

impl<T> KeepAlive<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame, SinkError = ConnectionError>,
{
    pub fn new(inner: T, timer: Timer, payload: Bytes, ttl: Duration) -> KeepAlive<T> {
        KeepAlive {
            inner,
            timer,
            ttl,
            state: None,
        }
    }
}

/// > Receivers of a PING frame that does not include an ACK flag MUST send
/// > a PING frame with the ACK flag set in response, with an identical
/// > payload. PING responses SHOULD be given higher priority than any
/// > other frame.
impl<T> Stream for KeepAlive<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame, SinkError = ConnectionError>,
{
    type Item = Frame;
    type Error = ConnectionError;

    /// Reads the next frame from the underlying socket.
    fn poll(&mut self) -> Poll<Option<Frame>, ConnectionError> {
        match self.inner.poll()? {
            Async::Ready(Some(Frame::Ping(ping))) => {
                if !ping.is_ack() {
                    Ok(Async::Ready(Some(Frame::Ping(ping))))
                } else {
                    let state = match self.state.take() {
                        Some(State::Waiting { deadline, remaining }) => {
                            if remaining > 1 {
                                State::Waiting { deadline, remaining: remaining - 1, }
                            } else {
                                let should_ping = self.timer.sleep(self.ttl / 2);
                                State::Idle { last: Instant::now(), should_ping }
                            }
                        }
                        None => {
                            let should_ping = self.timer.sleep(self.ttl / 2);
                            State::Idle { last: Instant::now(), should_ping }                        
                        }
                        Some(s) => {
                            trace!("unexpected pong received state={:?}", s);
                            s
                        }
                    };
                    self.state = Some(state);
                    self.poll()
                }
            }

            poll => Ok(poll),
        }
    }
}

impl<T> Sink for KeepAlive<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame, SinkError = ConnectionError>,
{
    type SinkItem = Frame;
    type SinkError = ConnectionError;

    fn start_send(&mut self, item: Frame) -> StartSend<Frame, ConnectionError> {
        self.inner.start_send(item)
    }

    fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
        match self.state.take() {
            None => {},
            Some(State::Idle { last, mut should_ping }) => {
                match should_ping.poll() {
                    Err(e) => {
                        error!("timer error: {}", e);
                    }
                    Ok(poll) => {
                        if poll.is_ready() {
                            // TODO generate an interesting payload to compare against on receipt?
                            let ping = Ping::ping(vec![0u8; 8].into());
                            if let AsyncSink::NotReady(ping) = self.start_send(ping.into())? {
                                let deadline = self.timer.sleep(self.ttl / 2);
                                self.state = Some(State::Pending { deadline, ping })
                            }
                        } else {
                            self.state = Some(State::Idle { last, should_ping });
                        }
                    }
                }
            }
            Some(State::Pending { mut deadline, ping }) => {
                match deadline.poll() {
                    Err(e) => {
                        error!("timer error: {}", e);
                    }
                    Ok(poll) => {
                        if poll.is_ready() {
                            // We haven't received enough pings by the deadline...
                            unimplemented!()
                        }
                    }
                }
                if let AsyncSink::NotReady(ping) = self.inner.start_send(ping)? {
                    unimplemented!();
                }
                let should_ping = self.timer.sleep(self.ttl / 2);
                let last = Instant::now();
                self.state = Some(State::Idle { last, should_ping });

            }
            Some(State::Waiting { deadline, remaining }) => {
                unimplemented!();
            }
        }
        
        self.inner.poll_complete()
    }
}

impl<T> ReadySink for KeepAlive<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame, SinkError = ConnectionError>,
          T: ReadySink,
{
    fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        // XXX do we need to account for sleeps here?
        self.inner.poll_ready()
    }
}
