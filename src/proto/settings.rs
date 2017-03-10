use ConnectionError;
use frame::{self, Frame};

use futures::*;

pub struct Settings<T> {
    inner: T,
    // Our settings
    local: frame::SettingSet,
    // Peer settings
    remote: frame::SettingSet,
    // Number of acks remaining to send to the peer
    remaining_acks: usize,
    // True when the local settings must be flushed to the remote
    is_dirty: bool,
}

impl<T> Settings<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame, SinkError = ConnectionError>,
{
    pub fn new(inner: T) -> Settings<T> {
        Settings {
            inner: inner,
            local: frame::SettingSet::default(),
            remote: frame::SettingSet::default(),
            remaining_acks: 0,
            // Always start in the dirty state as sending the settings frame is
            // part of the connection handshake
            is_dirty: true,
        }
    }

    fn has_pending_sends(&self) -> bool {
        self.is_dirty || self.remaining_acks > 0
    }

    fn try_send_pending(&mut self) -> Poll<(), ConnectionError> {
        if self.is_dirty {
            // Create the new frame
            let frame = frame::Settings::new(self.local.clone()).into();
            try_ready!(self.try_send(frame));

            self.is_dirty = false;
        }

        unimplemented!();
    }

    fn try_send(&mut self, item: frame::Frame) -> Poll<(), ConnectionError> {
        if let AsyncSink::NotReady(_) = try!(self.inner.start_send(item)) {
            // Ensure that call to `poll_complete` guarantee is called to satisfied
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
        self.inner.poll()
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
        if self.has_pending_sends() {
            // Try to flush them
            try!(self.poll_complete());

            if self.has_pending_sends() {
                return Ok(AsyncSink::NotReady(item));
            }
        }

        self.inner.start_send(item)
    }

    fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
        self.inner.poll_complete()
    }

    fn close(&mut self) -> Poll<(), ConnectionError> {
        if self.has_pending_sends() {
            try_ready!(self.poll_complete());
        }

        self.inner.close()
    }
}
