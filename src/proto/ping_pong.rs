use ConnectionError;
use frame::{Frame, Ping, SettingSet};
use proto::{ApplySettings, ControlPing, PingPayload, ReadySink};

use futures::*;

/// Acknowledges ping requests from the remote.
#[derive(Debug)]
pub struct PingPong<T, U> {
    inner: T,
    sending_pong: Option<Frame<U>>,
    received_pong: Option<PingPayload>,
    blocked_ping: Option<task::Task>,
    expecting_pong: bool,
}

impl<T, U> PingPong<T, U>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
{
    pub fn new(inner: T) -> Self {
        PingPong {
            inner,
            sending_pong: None,
            received_pong: None,
            expecting_pong: false,
            blocked_ping: None,
        }
    }
}

impl<T: ApplySettings, U> ApplySettings for PingPong<T, U> {
    fn apply_local_settings(&mut self, set: &SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_local_settings(set)
    }

    fn apply_remote_settings(&mut self, set: &SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_remote_settings(set)
    }
}

impl<T, U> ControlPing for PingPong<T, U>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ReadySink,
{
    fn start_ping(&mut self, body: PingPayload) -> StartSend<PingPayload, ConnectionError> {
        if self.inner.poll_ready()?.is_not_ready() {
            return Ok(AsyncSink::NotReady(body));
        }

        // Only allow one in-flight ping.
        if self.expecting_pong || self.received_pong.is_some() {
            self.blocked_ping = Some(task::current());
            return Ok(AsyncSink::NotReady(body))
        }

        match self.inner.start_send(Ping::ping(body).into())? {
            AsyncSink::NotReady(_) => {
                // By virtual of calling inner.poll_ready(), this must not happen.
                unreachable!()
            }
            AsyncSink::Ready => {
                self.expecting_pong = true;
                Ok(AsyncSink::Ready)
            }
        }
    }

    fn take_pong(&mut self) -> Option<PingPayload> {
        match self.received_pong.take() {
            None => None,
            Some(p) => {
                self.expecting_pong = false;
                if let Some(task) = self.blocked_ping.take() {
                    task.notify();
                }
                Some(p)
            }
        }
    }
}

impl<T, U> PingPong<T, U>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
{
    fn try_send_pong(&mut self) -> Poll<(), ConnectionError> {
        if let Some(pong) = self.sending_pong.take() {
            if let AsyncSink::NotReady(pong) = self.inner.start_send(pong)? {
                // If the pong can't be sent, save it.
                self.sending_pong = Some(pong);
                return Ok(Async::NotReady);
            }
        }
        Ok(Async::Ready(()))
    }
}

/// > Receivers of a PING frame that does not include an ACK flag MUST send
/// > a PING frame with the ACK flag set in response, with an identical
/// > payload. PING responses SHOULD be given higher priority than any
/// > other frame.
impl<T, U> Stream for PingPong<T, U>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
{
    type Item = Frame;
    type Error = ConnectionError;

    /// Reads the next frame from the underlying socket, eliding ping requests.
    ///
    /// If a PING is received without the ACK flag, the frame is sent to the remote with
    /// its ACK flag set.
    fn poll(&mut self) -> Poll<Option<Frame>, ConnectionError> {
        loop {
            // Don't read any frames until `inner` accepts any pending pong.
            try_ready!(self.try_send_pong());

            match self.inner.poll()? {
                Async::Ready(Some(Frame::Ping(ping))) => {
                    if ping.is_ack() {
                        // Save acknowledgements to be returned from take_pong().
                        self.received_pong = Some(ping.into_payload());
                        if let Some(task) = self.blocked_ping.take() {
                            task.notify();
                        }
                    } else {
                        // Save the ping's payload to be sent as an acknowledgement.
                        let pong = Ping::pong(ping.into_payload());
                        self.sending_pong = Some(pong.into());
                    }
                }

                // Everything other than ping gets passed through.
                f => return Ok(f),
            }
        }
    }
}

impl<T, U> Sink for PingPong<T, U>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
{
    type SinkItem = Frame<U>;
    type SinkError = ConnectionError;

    fn start_send(&mut self, item: Self::SinkItem)
        -> StartSend<Self::SinkItem, Self::SinkError>
    {
        // Pings _SHOULD_ have priority over other messages, so attempt to send pending
        // ping frames before attempting to send `item`.
        if self.try_send_pong()?.is_not_ready() {
            return Ok(AsyncSink::NotReady(item));
        }

        self.inner.start_send(item)
    }

    /// Polls the underlying sink and tries to flush pending pong frames.
    fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
        try_ready!(self.try_send_pong());
        self.inner.poll_complete()
    }
}

impl<T, U> ReadySink for PingPong<T, U>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ReadySink,
{
    fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        try_ready!(self.try_send_pong());
        self.inner.poll_ready()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use proto::ControlPing;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    #[test]
    fn responds_to_ping_with_pong() {
        let trans = Transport::default();
        let mut ping_pong = PingPong::new(trans.clone());

        {
            let mut trans = trans.0.borrow_mut();
            let ping = Ping::ping(*b"buoyant_");
            trans.from_socket.push_back(ping.into());
        }

        match ping_pong.poll() {
            Ok(Async::NotReady) => {} // cool
            rsp => panic!("unexpected poll result: {:?}", rsp),
        }

        {
            let mut trans = trans.0.borrow_mut();
            assert_eq!(trans.to_socket.len(), 1);
            match trans.to_socket.pop_front().unwrap() {
                Frame::Ping(pong) => {
                    assert!(pong.is_ack());
                    assert_eq!(&pong.into_payload(), b"buoyant_");
                }
                f => panic!("unexpected frame: {:?}", f),
            }
        }
    }

    #[test]
    fn responds_to_ping_even_when_blocked() {
        let trans = Transport::default();
        let mut ping_pong = PingPong::new(trans.clone());

        // Configure the transport so that writes can't proceed.
        {
            let mut trans = trans.0.borrow_mut();
            trans.start_send_blocked = true;
        }

        // The transport receives a ping but can't send it immediately.
        {
            let mut trans = trans.0.borrow_mut();
            let ping = Ping::ping(*b"buoyant?");
            trans.from_socket.push_back(ping.into());
        }
        assert!(ping_pong.poll().unwrap().is_not_ready());

        // The transport receives another ping but can't send it immediately.
        {
            let mut trans = trans.0.borrow_mut();
            let ping = Ping::ping(*b"buoyant!");
            trans.from_socket.push_back(ping.into());
        }
        assert!(ping_pong.poll().unwrap().is_not_ready());

        // At this point, ping_pong is holding two pongs that it cannot send.
        {
            let mut trans = trans.0.borrow_mut();
            assert!(trans.to_socket.is_empty());

            trans.start_send_blocked = false;
        }

        // Now that start_send_blocked is disabled, the next poll will successfully send
        // the pongs on the transport.
        assert!(ping_pong.poll().unwrap().is_not_ready());
        {
            let mut trans = trans.0.borrow_mut();
            assert_eq!(trans.to_socket.len(), 2);
            match trans.to_socket.pop_front().unwrap() {
                Frame::Ping(pong) => {
                    assert!(pong.is_ack());
                    assert_eq!(&pong.into_payload(), b"buoyant?");
                }
                f => panic!("unexpected frame: {:?}", f),
            }
            match trans.to_socket.pop_front().unwrap() {
                Frame::Ping(pong) => {
                    assert!(pong.is_ack());
                    assert_eq!(&pong.into_payload(), b"buoyant!");
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
            let pong = Ping::pong(*b"buoyant!");
            trans.from_socket.push_back(pong.into());
        }

        assert!(ping_pong.poll().unwrap().is_not_ready());
        match ping_pong.take_pong() {
            Some(pong) => assert_eq!(&pong, b"buoyant!"),
            None => panic!("no pong received"),
        }

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

    impl ReadySink for Transport {
        fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
            let trans = self.0.borrow();
            if trans.closing || trans.start_send_blocked {
                Ok(Async::NotReady)
            } else {
                Ok(Async::Ready(()))
            }
        }
    }
}
