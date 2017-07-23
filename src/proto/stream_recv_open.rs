use ConnectionError;
use error::Reason::{ProtocolError, RefusedStream};
use frame::{Frame, StreamId};
use proto::*;

/// Ensures that frames are received on open streams in the appropriate state.
#[derive(Debug)]
pub struct StreamRecvOpen<T> {
    inner: T,
    max_concurrency: Option<u32>,
    initial_window_size: WindowSize,
    pending_refuse: Option<StreamId>,
}

impl<T> StreamRecvOpen<T> {

    pub fn new<U>(initial_window_size: WindowSize,
                  max_concurrency: Option<u32>,
                  inner: T)
            -> StreamRecvOpen<T>
        where T: Stream<Item = Frame, Error = ConnectionError>,
            T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
            T: ControlStreams,
    {
        StreamRecvOpen {
            inner,
            max_concurrency,
            initial_window_size,
            pending_refuse: None,
        }
    }

}

impl<T, U> StreamRecvOpen<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams,
{
    fn send_refuse(&mut self, id: StreamId) -> Poll<(), ConnectionError> {
        debug_assert!(self.pending_refuse.is_none());

        let f = frame::Reset::new(id, RefusedStream);
        match self.inner.start_send(f.into())? {
            AsyncSink::Ready => {
                self.inner.reset_stream(id, RefusedStream);
                Ok(Async::Ready(()))
            }
            AsyncSink::NotReady(_) => {
                self.pending_refuse = Some(id);
                Ok(Async::NotReady)
            }
        }
    }

    fn send_pending_refuse(&mut self) -> Poll<(), ConnectionError> {
        if let Some(id) = self.pending_refuse.take() {
            try_ready!(self.send_refuse(id));
        }
        Ok(Async::Ready(()))
    }
}

/// Handles updates to `SETTINGS_MAX_CONCURRENT_STREAMS` from the local peer.
impl<T> ApplySettings for StreamRecvOpen<T>
    where T: ApplySettings
{
    fn apply_local_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.max_concurrency = set.max_concurrent_streams();
        self.initial_window_size = set.initial_window_size();
        self.inner.apply_local_settings(set)
    }

    fn apply_remote_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_remote_settings(set)
    }
}

/// Helper.
impl<T: ControlStreams> StreamRecvOpen<T> {
    fn check_not_reset(&self, id: StreamId) -> Result<(), ConnectionError> {
        // Ensure that the stream hasn't been closed otherwise.
        match self.inner.get_reset(id) {
            Some(reason) => Err(reason.into()),
            None => Ok(()),
        }
    }
}

/// Ensures that frames are received on open streams in the appropriate state.
impl<T, U> Stream for StreamRecvOpen<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams,
{
    type Item = T::Item;
    type Error = T::Error;

    fn poll(&mut self) -> Poll<Option<T::Item>, T::Error> {
        // Since there's only one slot for pending refused streams, it must be cleared
        // before polling  a frame from the transport.
        try_ready!(self.send_pending_refuse());

        trace!("poll");
        loop {
            let frame = match try_ready!(self.inner.poll()) {
                None => return Ok(Async::Ready(None)),
                Some(f) => f,
            };

            let id = frame.stream_id();
            trace!("poll: id={:?}", id);

            if id.is_zero() {
                if !frame.is_connection_frame() {
                    return Err(ProtocolError.into())
                }

                // Nothing to do on connection frames.
                return Ok(Async::Ready(Some(frame)));
            }

            match &frame {
                &Frame::Reset(..) => {}

                &Frame::Headers(..) => {
                    self.check_not_reset(id)?;

                    if T::remote_valid_id(id) {
                        if self.inner.is_remote_active(id) {
                            // Can't send a a HEADERS frame on a remote stream that's
                            // active, because we've already received headers.  This will
                            // have to change to support PUSH_PROMISE.
                            return Err(ProtocolError.into());
                        }

                        if !T::remote_can_open() {
                            return Err(ProtocolError.into());
                        }

                        if let Some(max) = self.max_concurrency {
                            if (max as usize) < self.inner.remote_active_len() {
                                debug!("refusing stream that would exceed max_concurrency={}", max);
                                self.send_refuse(id)?;

                                // There's no point in returning an error to the application.
                                continue;
                            }
                        }

                        self.inner.remote_open(id, self.initial_window_size)?;
                    } else {
                        // On remote streams,
                        self.inner.local_open_recv_half(id, self.initial_window_size)?;
                    }
                }

                // All other stream frames are sent only when
                _ => {
                    self.check_not_reset(id)?;
                    if !self.inner.is_recv_open(id) {
                        return Err(ProtocolError.into());
                    }
                }
            }

            // If the frame ends the stream, it will be handled in
            // StreamRecvClose.
            return Ok(Async::Ready(Some(frame)));
        }
    }
}

/// Sends pending resets before operating on the underlying transport.
impl<T, U> Sink for StreamRecvOpen<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams,
{
    type SinkItem = T::SinkItem;
    type SinkError = T::SinkError;

    fn start_send(&mut self, frame: T::SinkItem) -> StartSend<T::SinkItem, T::SinkError> {
        // The local must complete refusing the remote stream before sending any other
        // frames.
        if self.send_pending_refuse()?.is_not_ready() {
            return Ok(AsyncSink::NotReady(frame));
        }

        self.inner.start_send(frame)
    }

    fn poll_complete(&mut self) -> Poll<(), T::SinkError> {
        try_ready!(self.send_pending_refuse());
        self.inner.poll_complete()
    }
}

/// Sends pending resets before checking the underlying transport's readiness.
impl<T, U> ReadySink for StreamRecvOpen<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ReadySink,
          T: ControlStreams,
{
    fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        if let Some(id) = self.pending_refuse.take() {
            try_ready!(self.send_refuse(id));
        }

        self.inner.poll_ready()
    }
}

/// Proxy.
impl<T: ControlStreams> ControlStreams for StreamRecvOpen<T> {
    fn local_valid_id(id: StreamId) -> bool {
        T::local_valid_id(id)
    }

    fn remote_valid_id(id: StreamId) -> bool {
        T::remote_valid_id(id)
    }

    fn local_can_open() -> bool {
        T::local_can_open()
    }

    fn local_open(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError> {
        self.inner.local_open(id, sz)
    }

    fn remote_open(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError> {
        self.inner.remote_open(id, sz)
    }

    fn local_open_recv_half(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError> {
        self.inner.local_open_recv_half(id, sz)
    }

    fn remote_open_send_half(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError> {
        self.inner.remote_open_send_half(id, sz)
    }

    fn close_send_half(&mut self, id: StreamId) -> Result<(), ConnectionError> {
        self.inner.close_send_half(id)
    }

    fn close_recv_half(&mut self, id: StreamId) -> Result<(), ConnectionError> {
        self.inner.close_recv_half(id)
    }

    fn reset_stream(&mut self, id: StreamId, cause: Reason) {
        self.inner.reset_stream(id, cause)
    }

    fn get_reset(&self, id: StreamId) -> Option<Reason> {
        self.inner.get_reset(id)
    }

    fn is_local_active(&self, id: StreamId) -> bool {
        self.inner.is_local_active(id)
    }

    fn is_remote_active(&self, id: StreamId) -> bool {
        self.inner.is_remote_active(id)
    }

    fn local_active_len(&self) -> usize {
        self.inner.local_active_len()
    }

    fn remote_active_len(&self) -> usize {
        self.inner.remote_active_len()
    }

    fn update_inital_recv_window_size(&mut self, old_sz: WindowSize, new_sz: WindowSize) {
        self.inner.update_inital_recv_window_size(old_sz, new_sz)
    }

    fn update_inital_send_window_size(&mut self, old_sz: WindowSize, new_sz: WindowSize) {
        self.inner.update_inital_send_window_size(old_sz, new_sz)
    }

    fn recv_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState> {
        self.inner.recv_flow_controller(id)
    }

    fn send_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState> {
        self.inner.send_flow_controller(id)
    }

    fn is_send_open(&mut self, id: StreamId) -> bool {
        self.inner.is_send_open(id)
    }

    fn is_recv_open(&mut self, id: StreamId) -> bool  {
        self.inner.is_recv_open(id)
    }
}

/// Proxy.
impl<T: ControlPing> ControlPing for StreamRecvOpen<T> {
    fn start_ping(&mut self, body: PingPayload) -> StartSend<PingPayload, ConnectionError> {
        self.inner.start_ping(body)
    }

    fn take_pong(&mut self) -> Option<PingPayload> {
        self.inner.take_pong()
    }
}
