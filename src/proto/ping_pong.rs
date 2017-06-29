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

                // Anything other than ping gets passed through.
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
        if self.pending_pongs.is_empty() {
            return Ok(poll);
        }

        // Then, try to flush pending pongs. Even if poll is not ready, we may be able to
        // start sending pongs.
        while let Some(pong) = self.pending_pongs.pop_front() {
            if let AsyncSink::NotReady(pong) = self.inner.start_send(pong)? {
                // If we can't flush all of the pongs, we're not ready. Save the pong to
                // be sent next time.
                self.pending_pongs.push_front(pong);
                return Ok(Async::NotReady);
            }
        }

        // Because we've invoked start_send, poll_complete needs to be invoked again.
        self.inner.poll_complete()
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

#[cfg(test)]
mod test {
    use super::*;
    use bytes::Bytes;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn responds_to_ping_with_pong() {
        let trans = Transport::default();
        let mut ping_pong = PingPong::new(trans.clone());

        {
            let mut trans = trans.0.borrow_mut();
            let ping = Ping::ping(Bytes::from_static(b"buoyant!"));
            trans.from_socket.push_back(ping.into());
        }

        match ping_pong.poll() {
            Ok(Async::NotReady) => {} // cool
            rsp => panic!("unexpected poll result: {:?}", rsp),
        }

        assert!(ping_pong.poll_complete().is_ok());
        {
            let mut trans = trans.0.borrow_mut();
            assert_eq!(trans.to_socket.len(), 1);
            match trans.to_socket.pop_front().unwrap() {
                Frame::Ping(pong) => {
                    assert!(pong.is_ack());
                    assert_eq!(pong.into_payload(), Bytes::from_static(b"buoyant!"));
                }
                f => panic!("unexpected frame: {:?}", f),
            }
        }
    }

    #[test]
    fn responds_to_ping_even_when_blocked() {
        let trans = Transport::default();
        let mut ping_pong = PingPong::new(trans.clone());

        {
            let mut trans = trans.0.borrow_mut();
            trans.start_send_blocked = true;
        }

        {
            let mut trans = trans.0.borrow_mut();
            let ping = Ping::ping(Bytes::from_static(b"buoyant!"));
            trans.from_socket.push_back(ping.into());
        }

        match ping_pong.poll() {
            Ok(Async::NotReady) => {} // cool
            rsp => panic!("unexpected poll result: {:?}", rsp),
        }

        {
            let mut trans = trans.0.borrow_mut();
            let ping = Ping::ping(Bytes::from_static(b"buoyant!"));
            trans.from_socket.push_back(ping.into());
        }

        match ping_pong.poll() {
            Ok(Async::NotReady) => {} // cool
            rsp => panic!("unexpected poll result: {:?}", rsp),
        }
        assert!(ping_pong.poll_complete().unwrap().is_not_ready());

        {
            let mut trans = trans.0.borrow_mut();
            assert!(trans.to_socket.is_empty());

            trans.start_send_blocked = false;
        }

        match ping_pong.poll() {
            Ok(Async::NotReady) => {} // cool
            rsp => panic!("unexpected poll result: {:?}", rsp),
        }
        assert!(ping_pong.poll_complete().unwrap().is_not_ready());


        {
            let mut trans = trans.0.borrow_mut();
            assert_eq!(trans.to_socket.len(), 2);
            match trans.to_socket.pop_front().unwrap() {
                Frame::Ping(pong) => {
                    assert!(pong.is_ack());
                    assert_eq!(pong.into_payload(), Bytes::from_static(b"buoyant!"));
                }
                f => panic!("unexpected frame: {:?}", f),
            }
            match trans.to_socket.pop_front().unwrap() {
                Frame::Ping(pong) => {
                    assert!(pong.is_ack());
                    assert_eq!(pong.into_payload(), Bytes::from_static(b"buoyant!"));
                }
                f => panic!("unexpected frame: {:?}", f),
            }
        }
    }

    #[test]
    fn pong_passes_through() {
        let trans = Transport::default();
        let mut ping_pong = PingPong::new(trans.clone());

        {
            let mut trans = trans.0.borrow_mut();
            let pong = Ping::pong(Bytes::from_static(b"buoyant!"));
            trans.from_socket.push_back(pong.into());
        }

        match ping_pong.poll().unwrap() {
            Async::Ready(Some(Frame::Ping(pong))) => {
                assert!(pong.is_ack());
                assert_eq!(pong.into_payload(), Bytes::from_static(b"buoyant!"));
            }
            f => panic!("unexpected frame: {:?}", f),
        }

        assert!(ping_pong.poll_complete().is_ok());
        {
            let trans = trans.0.borrow();
            assert_eq!(trans.to_socket.len(), 0);
        }
    }

    /// A stubbed transport for tests.a
    ///
    /// We probably want/have something generic for this?
    #[derive(Clone, Default)]
    struct Transport(Rc<RefCell<Inner>>);

    #[derive(Default)]
    struct Inner {
        from_socket: VecDeque<Frame>,
        to_socket: VecDeque<Frame>,
        read_blocked: bool,
        start_send_blocked: bool,
        closing: bool,
    }

    impl Stream for Transport {
        type Item = Frame;
        type Error = ConnectionError;

        fn poll(&mut self) -> Poll<Option<Frame>, ConnectionError> {
            let mut trans = self.0.borrow_mut();
            if trans.read_blocked || (!trans.closing && trans.from_socket.is_empty()) {
                Ok(Async::NotReady)
            } else {
                Ok(trans.from_socket.pop_front().into())
            }
        }
    }

    impl Sink for Transport {
        type SinkItem = Frame;
        type SinkError = ConnectionError;

        fn start_send(&mut self, item: Frame) -> StartSend<Frame, ConnectionError> {
            let mut trans = self.0.borrow_mut();
            if trans.closing || trans.start_send_blocked {
                Ok(AsyncSink::NotReady(item))
            } else {
                trans.to_socket.push_back(item);
                Ok(AsyncSink::Ready)
            }
        }

        fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
            let trans = self.0.borrow();
            if !trans.to_socket.is_empty() {
                Ok(Async::NotReady)
            } else {
                Ok(Async::Ready(()))
            }
        }

        fn close(&mut self) -> Poll<(), ConnectionError> {
            {
                let mut trans = self.0.borrow_mut();
                trans.closing = true;
            }
            self.poll_complete()
        }
    }
}
