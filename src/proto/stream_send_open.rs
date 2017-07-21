use ConnectionError;
use error::User::{InvalidStreamId, StreamReset, Rejected};
use frame::{Frame, SettingSet};
use proto::*;

#[derive(Debug)]
pub struct StreamSendOpen<T> {
    inner: T,

    max_concurrency: Option<u32>,
    initial_window_size: WindowSize,
}

impl<T, U> StreamSendOpen<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams,
{
    pub fn new(initial_window_size: WindowSize,
               max_concurrency: Option<u32>,
               inner: T)
            -> StreamSendOpen<T>
    {
        StreamSendOpen {
            inner,
            max_concurrency,
            initial_window_size,
        }
    }
}

/// Handles updates to `SETTINGS_MAX_CONCURRENT_STREAMS`.
///
/// > Indicates the maximum number of concurrent streams that the senderg will allow. This
/// > limit is directional: it applies to the number of streams that the sender permits
/// > the receiver to create. Initially, there is no limit to this value. It is
/// > recommended that this value be no smaller than 100, so as to not unnecessarily limit
/// > parallelism.
/// >
/// > A value of 0 for SETTINGS_MAX_CONCURRENT_STREAMS SHOULD NOT be treated as special by
/// > endpoints. A zero value does prevent the creation of new streams; however, this can
/// > also happen for any limit that is exhausted with active streams. Servers SHOULD only
/// > set a zero value for short durations; if a server does not wish to accept requests,
/// > closing the connection is more appropriate.
///
/// > An endpoint that wishes to reduce the value of SETTINGS_MAX_CONCURRENT_STREAMS to a
/// > value that is below the current number of open streams can either close streams that
/// > exceed the new value or allow streams to complete.
///
/// This module does NOT close streams when the setting changes.
impl<T: ApplySettings> ApplySettings for StreamSendOpen<T> {
    fn apply_local_settings(&mut self, set: &SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_local_settings(set)
    }

    fn apply_remote_settings(&mut self, set: &SettingSet) -> Result<(), ConnectionError> {
        self.max_concurrency = set.max_concurrent_streams();
        self.initial_window_size = set.initial_window_size();
        self.inner.apply_remote_settings(set)
    }
}

impl<T> Stream for StreamSendOpen<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: ControlStreams,
{
    type Item = Frame;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Option<Frame>, ConnectionError> {
        self.inner.poll()
    }
}

impl<T, U> Sink for StreamSendOpen<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams,
{
    type SinkItem = T::SinkItem;
    type SinkError = T::SinkError;

    fn start_send(&mut self, frame: T::SinkItem) -> StartSend<T::SinkItem, T::SinkError> {
        use frame::Frame::*;

        let id = frame.stream_id();
        if id.is_zero() {
            // Nothing to do on connection frames.
            return self.inner.start_send(frame);
        }

        // Reset the stream immediately and send the Reset on the underlying transport.
        if let Reset(rst) = frame {
            self.inner.reset_stream(id, rst.reason());
            return self.inner.start_send(Reset(rst));
        }

        // Ensure that the stream hasn't been closed otherwise.
        if let Some(reason) = self.inner.get_reset(id) {
            return Err(StreamReset(reason).into())
        }

        if T::local_valid_id(id) {
            if !self.inner.is_local_active(id) {
                if !T::local_can_open() {
                    return Err(InvalidStreamId.into());
                }

                if let Some(max) = self.max_concurrency {
                    if (max as usize) < self.inner.local_active_len() {
                        return Err(Rejected.into());
                    }
                }

                self.inner.local_open(id, self.initial_window_size)?;
            }
        } else {
            // If the frame was part of a remote stream, it MUST already exist.
            if !self.inner.is_remote_active(id) && !frame.is_reset() {
                return Err(InvalidStreamId.into());
            }
        }

        if let &Data(..) = &frame {
            self.inner.check_can_send_data(id)?;
        }

        return self.inner.start_send(frame);
    }

    fn poll_complete(&mut self) -> Poll<(), T::SinkError> {
        self.inner.poll_complete()
    }
}

impl<T, U> ReadySink for StreamSendOpen<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams,
          T: ReadySink,
{
    fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        self.inner.poll_ready()
    }
}

impl<T: ControlStreams> ControlStreams for StreamSendOpen<T> {
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

    fn close_local_half(&mut self, id: StreamId) -> Result<(), ConnectionError> {
        self.inner.close_local_half(id)
    }

    fn close_remote_half(&mut self, id: StreamId) -> Result<(), ConnectionError> {
        self.inner.close_remote_half(id)
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

    fn local_update_inital_window_size(&mut self, old_sz: u32, new_sz: u32) {
        self.inner.local_update_inital_window_size(old_sz, new_sz)
    }

    fn remote_update_inital_window_size(&mut self, old_sz: u32, new_sz: u32) {
        self.inner.remote_update_inital_window_size(old_sz, new_sz)
    }

    fn local_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState> {
        self.inner.local_flow_controller(id)
    }

    fn remote_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState> {
        self.inner.remote_flow_controller(id)
    }

    fn check_can_send_data(&mut self, id: StreamId) -> Result<(), ConnectionError> {
        self.inner.check_can_send_data(id)
    }

    fn check_can_recv_data(&mut self, id: StreamId) -> Result<(), ConnectionError>  {
        self.inner.check_can_recv_data(id)
    }
}

impl<T: ControlFlow> ControlFlow for StreamSendOpen<T> {
    fn poll_window_update(&mut self) -> Poll<WindowUpdate, ConnectionError> {
        self.inner.poll_window_update()
    }

    fn expand_window(&mut self, id: StreamId, incr: WindowSize) -> Result<(), ConnectionError> {
        self.inner.expand_window(id, incr)
    }
}

impl<T: ControlPing> ControlPing for StreamSendOpen<T> {
    fn start_ping(&mut self, body: PingPayload) -> StartSend<PingPayload, ConnectionError> {
        self.inner.start_ping(body)
    }

    fn take_pong(&mut self) -> Option<PingPayload> {
        self.inner.take_pong()
    }
}
