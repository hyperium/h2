use {StreamId, ConnectionError};
use frame::{self, Frame, SettingSet};
use proto::*;

use tokio_io::AsyncRead;
use bytes::BufMut;

use std::io;

/// Exposes settings to "upper" layers of the transport (i.e. from Settings up to---and
/// above---Connection).
pub trait ControlSettings {
    fn update_local_settings(&mut self, set: frame::SettingSet) -> Result<(), ConnectionError>;

    fn remote_push_enabled(&self) -> Option<bool>;
    fn remote_max_concurrent_streams(&self) -> Option<u32>;
    fn remote_initial_window_size(&self) -> WindowSize;
}

/// Allows settings updates to be pushed "down" the transport (i.e. from Settings down to
/// FramedWrite).
pub trait ApplySettings {
    fn apply_local_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError>;
    fn apply_remote_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError>;
}

#[derive(Debug)]
pub struct Settings<T> {
    // Upstream transport
    inner: T,

    remote_push_enabled: Option<bool>,
    remote_max_concurrent_streams: Option<u32>,
    remote_initial_window_size: WindowSize,

    // Number of acks remaining to send to the peer
    remaining_acks: usize,

    // Holds a new set of local values to be applied.
    pending_local: Option<SettingSet>,

    // True when we have received a settings frame from the remote.
    received_remote: bool,
}

impl<T, U> Settings<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
{
    pub fn new(inner: T, local: SettingSet) -> Settings<T> {
        Settings {
            inner: inner,
            pending_local: Some(local),
            remote_push_enabled: None,
            remote_max_concurrent_streams: None,
            remote_initial_window_size: 65_535,
            remaining_acks: 0,
            received_remote: false,
        }
    }

    /// Swap the inner transport while maintaining the current state.
    pub fn swap_inner<T2, F: FnOnce(T) -> T2>(self, f: F) -> Settings<T2> {
        let inner = f(self.inner);

        Settings {
            inner: inner,
            remote_push_enabled: self.remote_push_enabled,
            remote_max_concurrent_streams: self.remote_max_concurrent_streams,
            remote_initial_window_size: self.remote_initial_window_size,
            remaining_acks: self.remaining_acks,
            pending_local: self.pending_local,
            received_remote: self.received_remote,
        }
    }

    fn try_send_pending(&mut self) -> Poll<(), ConnectionError> {
        trace!("try_send_pending; dirty={} acks={}", self.pending_local.is_some(), self.remaining_acks);
        if let Some(local) = self.pending_local.take() {
            try_ready!(self.try_send_local(local));
        }

        while self.remaining_acks > 0 {
            let frame = frame::Settings::ack().into();
            try_ready!(self.try_send(frame));

            self.remaining_acks -= 1;
        }

        Ok(Async::Ready(()))
    }

    fn try_send_local(&mut self, local: SettingSet) -> Poll<(), ConnectionError> {
        let frame = frame::Settings::new(local.clone()).into();
        if self.try_send(frame)?.is_not_ready() {
            self.pending_local = Some(local);
            Ok(Async::NotReady)
        } else {
            Ok(Async::Ready(()))
        }
    }

    fn try_send(&mut self, frame: frame::Settings) -> Poll<(), ConnectionError> {
        trace!("try_send");
        if self.inner.start_send(frame.into())?.is_ready() {
            Ok(Async::Ready(()))
        } else {
            Ok(Async::NotReady)
        }
    }
}

impl<T, U> ControlSettings for Settings<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
{
    fn update_local_settings(&mut self, local: SettingSet) -> Result<(), ConnectionError> {
        self.try_send_local(local)?;
        Ok(())
    }

    fn remote_initial_window_size(&self) -> u32 {
        self.remote_initial_window_size
    }

    fn remote_max_concurrent_streams(&self) -> Option<u32> {
        self.remote_max_concurrent_streams
    }

    fn remote_push_enabled(&self) -> Option<bool> {
        self.remote_push_enabled
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

                        if let Some(sz) = settings.initial_window_size() {
                            self.remote_initial_window_size = sz;
                        }
                        if let Some(max) = settings.max_concurrent_streams() {
                            self.remote_max_concurrent_streams = Some(max);
                        }
                        if let Some(ok) = settings.enable_push() {
                            self.remote_push_enabled = Some(ok);
                        }

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
