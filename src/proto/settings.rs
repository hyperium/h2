use ConnectionError;
use frame::{self, Frame};
use proto::ReadySink;

use futures::*;

#[derive(Debug)]
pub struct Settings<T> {
    // Upstream transport
    inner: T,

    // Our settings
    local: frame::SettingSet,

    // Peer settings
    remote: frame::SettingSet,

    // Number of acks remaining to send to the peer
    remaining_acks: usize,

    // True when the local settings must be flushed to the remote
    is_dirty: bool,

    // True when we have received a settings frame from the remote.
    received_remote: bool,
}

impl<T> Settings<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame, SinkError = ConnectionError>,
{
    pub fn new(inner: T, local: frame::SettingSet) -> Settings<T> {
        Settings {
            inner: inner,
            local: local,
            remote: frame::SettingSet::default(),
            remaining_acks: 0,
            is_dirty: true,
            received_remote: false,
        }
    }

    fn try_send_pending(&mut self) -> Poll<(), ConnectionError> {
        if self.is_dirty {
            let frame = frame::Settings::new(self.local.clone());
            try_ready!(self.try_send(frame));

            self.is_dirty = false;
        }

        while self.remaining_acks > 0 {
            let frame = frame::Settings::ack().into();
            try_ready!(self.try_send(frame));

            self.remaining_acks -= 1;
        }

        Ok(Async::Ready(()))
    }

    fn try_send(&mut self, item: frame::Settings) -> Poll<(), ConnectionError> {
        if let AsyncSink::NotReady(_) = try!(self.inner.start_send(item.into())) {
            // TODO: I don't think this is needed actually... It was originally
            // done to "satisfy the start_send" contract...
            try!(self.inner.poll_complete());

            return Ok(Async::NotReady);
        }

        Ok(Async::Ready(()))
    }
}

impl<T> Stream for Settings<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame, SinkError = ConnectionError>,
{
    type Item = Frame;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Option<Frame>, ConnectionError> {
        loop {
            match try_ready!(self.inner.poll()) {
                Some(Frame::Settings(v)) => {
                    if v.is_ack() {
                        debug!("received remote settings ack");
                        // TODO: Handle acks
                    } else {
                        // Received new settings, queue an ACK
                        self.remaining_acks += 1;

                        // Save off the settings
                        self.remote = v.into_set();

                        let _ = try!(self.try_send_pending());
                    }
                }
                v => return Ok(Async::Ready(v)),
            }
        }
    }
}

impl<T> Sink for Settings<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame, SinkError = ConnectionError>,
{
    type SinkItem = Frame;
    type SinkError = ConnectionError;

    fn start_send(&mut self, item: Frame) -> StartSend<Frame, ConnectionError> {
        // Settings frames take priority, so `item` cannot be sent if there are
        // any pending acks OR the local settings have been changed w/o sending
        // an associated frame.
        if !try!(self.try_send_pending()).is_ready() {
            return Ok(AsyncSink::NotReady(item));
        }

        self.inner.start_send(item)
    }

    fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
        trace!("Settings::poll_complete");
        try_ready!(self.try_send_pending());
        self.inner.poll_complete()
    }

    fn close(&mut self) -> Poll<(), ConnectionError> {
        try_ready!(self.try_send_pending());
        self.inner.close()
    }
}

impl<T> ReadySink for Settings<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame, SinkError = ConnectionError>,
          T: ReadySink,
{
    fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        if try!(self.try_send_pending()).is_ready() {
            return self.inner.poll_ready();
        }

        Ok(Async::NotReady)
    }
}
