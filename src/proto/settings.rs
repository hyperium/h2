use ConnectionError;
use frame::{self, Frame};
use proto::ReadySink;

use futures::*;
use tokio_io::AsyncRead;
use bytes::BufMut;

use std::io;

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

impl<T, U> Settings<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
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

    /// Swap the inner transport while maintaining the current state.
    pub fn swap_inner<T2, F: FnOnce(T) -> T2>(self, f: F) -> Settings<T2> {
        let inner = f(self.inner);

        Settings {
            inner: inner,
            local: self.local,
            remote: self.remote,
            remaining_acks: self.remaining_acks,
            is_dirty: self.is_dirty,
            received_remote: self.received_remote,
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

impl<T, U> Stream for Settings<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
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

impl<T, U> Sink for Settings<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
{
    type SinkItem = Frame<U>;
    type SinkError = ConnectionError;

    fn start_send(&mut self, item: Self::SinkItem)
        -> StartSend<Self::SinkItem, Self::SinkError>
    {
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

impl<T, U> ReadySink for Settings<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ReadySink,
{
    fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        if try!(self.try_send_pending()).is_ready() {
            return self.inner.poll_ready();
        }

        Ok(Async::NotReady)
    }
}

impl<T: io::Read> io::Read for Settings<T> {
    fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        self.inner.read(dst)
    }
}

impl<T: AsyncRead> AsyncRead for Settings<T> {
    fn read_buf<B: BufMut>(&mut self, buf: &mut B) -> Poll<usize, io::Error>
        where Self: Sized,
    {
        self.inner.read_buf(buf)
    }

    unsafe fn prepare_uninitialized_buffer(&self, buf: &mut [u8]) -> bool {
        self.inner.prepare_uninitialized_buffer(buf)
    }
}
