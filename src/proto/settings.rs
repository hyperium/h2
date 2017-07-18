use {StreamId, ConnectionError};
use frame::{self, Frame, SettingSet};
use proto::*;

use tokio_io::AsyncRead;
use bytes::BufMut;

use std::io;


// TODO 
#[derive(Debug)]
pub struct Settings<T> {
    // Upstream transport
    inner: T,

    // Our settings
    local: SettingSet,

    // Peer settings
    remote: SettingSet,

    // Number of acks remaining to send to the peer
    remaining_acks: usize,

    // True when the local settings must be flushed to the remote
    is_local_dirty: bool,

    // True when we have received a settings frame from the remote.
    received_remote: bool,
}

impl<T, U> Settings<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
{
    pub fn new(inner: T, local: SettingSet) -> Settings<T> {
        Settings {
            inner: inner,
            local: local,
            remote: SettingSet::default(),
            remaining_acks: 0,
            is_local_dirty: true,
            received_remote: false,
        }
    }

    pub fn local_settings(&self) -> &SettingSet {
        &self.local
    }

    pub fn remote_settings(&self) -> &SettingSet {
        &self.local
    }

    /// Swap the inner transport while maintaining the current state.
    pub fn swap_inner<T2, F: FnOnce(T) -> T2>(self, f: F) -> Settings<T2> {
        let inner = f(self.inner);

        Settings {
            inner: inner,
            local: self.local,
            remote: self.remote,
            remaining_acks: self.remaining_acks,
            is_local_dirty: self.is_local_dirty,
            received_remote: self.received_remote,
        }
    }

    fn try_send_pending(&mut self) -> Poll<(), ConnectionError> {
        trace!("try_send_pending; dirty={} acks={}", self.is_local_dirty, self.remaining_acks);
        if self.is_local_dirty {
            let frame = frame::Settings::new(self.local.clone());
            try_ready!(self.try_send(frame));

            self.is_local_dirty = false;
        }

        while self.remaining_acks > 0 {
            let frame = frame::Settings::ack().into();
            try_ready!(self.try_send(frame));

            self.remaining_acks -= 1;
        }

        Ok(Async::Ready(()))
    }

    fn try_send(&mut self, item: frame::Settings) -> Poll<(), ConnectionError> {
        trace!("try_send");
        if self.inner.start_send(item.into())?.is_ready() {
            Ok(Async::Ready(()))
        } else {
            Ok(Async::NotReady)
        }
    }
}

impl<T: ControlStreams> ControlStreams for Settings<T> {
    fn streams(&self) -> &StreamMap {
        self.inner.streams()
    }

    fn streams_mut(&mut self) -> &mut StreamMap {
        self.inner.streams_mut()
    }

    fn stream_is_reset(&self, id: StreamId) -> Option<Reason> {
        self.inner.stream_is_reset(id)
    }
}

impl<T: ControlFlow> ControlFlow for Settings<T> {
    fn poll_window_update(&mut self) -> Poll<WindowUpdate, ConnectionError> {
        self.inner.poll_window_update()
    }

    fn expand_window(&mut self, id: StreamId, incr: WindowSize) -> Result<(), ConnectionError> {
        self.inner.expand_window(id, incr)
    }
}

impl<T: ControlPing> ControlPing for Settings<T> {
    fn start_ping(&mut self, body: PingPayload) -> StartSend<PingPayload, ConnectionError> {
        self.inner.start_ping(body)
    }

    fn take_pong(&mut self) -> Option<PingPayload> {
        self.inner.take_pong()
    }
}

impl<T> ControlSettings for Settings<T>{
    fn update_local_settings(&mut self, local: frame::SettingSet) -> Result<(), ConnectionError> {
        self.local = local;
        self.is_local_dirty = true;
        Ok(())
    }

    fn local_settings(&self) -> &SettingSet {
        &self.local
    }

    fn remote_settings(&self) -> &SettingSet {
        &self.remote
    }
}

impl<T, U> Stream for Settings<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ApplySettings,
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
                        // Apply the settings before saving them and sending
                        // acknowledgements.
                        let settings = v.into_set();
                        self.inner.apply_remote_settings(&settings)?;
                        self.remote = settings;

                        self.remaining_acks += 1;
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
        trace!("poll_complete");
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
        trace!("poll_ready");
        try_ready!(self.try_send_pending());
        self.inner.poll_ready()
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
