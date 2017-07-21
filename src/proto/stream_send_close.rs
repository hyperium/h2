use ConnectionError;
use error::Reason;
use frame::{self, Frame};
use proto::*;

// TODO track "last stream id" for GOAWAY.
// TODO track/provide "next" stream id.
// TODO reset_streams needs to be bounded.
// TODO track reserved streams (PUSH_PROMISE).

#[derive(Debug)]
pub struct StreamSendClose<T> {
    inner: T,
}

impl<T, U> StreamSendClose<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams,
{
    pub fn new(inner: T) -> StreamSendClose<T> {
        StreamSendClose { inner }
    }
}

impl<T> Stream for StreamSendClose<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: ControlStreams,
{
    type Item = Frame;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Option<Frame>, ConnectionError> {
        self.inner.poll()
    }
}

impl<T, U> Sink for StreamSendClose<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams,
{
    type SinkItem = Frame<U>;
    type SinkError = ConnectionError;

    fn start_send(&mut self, frame: Self::SinkItem) -> StartSend<Frame<U>, ConnectionError> {
        if frame.is_end_stream() {
            let id = frame.stream_id();
            if let &Frame::Reset(ref rst) = &frame {
                self.inner.reset_stream(id, rst.reason());
            } else {
                self.inner.close_stream_local_half(id)?;
            }
        }

        self.inner.start_send(frame)
    }

    fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
        self.inner.poll_complete()
    }
}

impl<T, U> ReadySink for StreamSendClose<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ReadySink,
          T: ControlStreams,
{
    fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        self.inner.poll_ready()
    }
}

impl<T: ApplySettings> ApplySettings for StreamSendClose<T> {
    fn apply_local_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_local_settings(set)
    }

    fn apply_remote_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_remote_settings(set)
    }
}

impl<T: ControlStreams> ControlStreams for StreamSendClose<T> {
    fn is_valid_local_id(id: StreamId) -> bool {
        T::is_valid_local_id(id)
    }

    fn is_valid_remote_id(id: StreamId) -> bool {
        T::is_valid_remote_id(id)
    }

    fn can_create_local_stream() -> bool {
        T::can_create_local_stream()
    }

    fn close_stream_local_half(&mut self, id: StreamId) -> Result<(), ConnectionError> {
        self.inner.close_stream_local_half(id)
    }

    fn close_stream_remote_half(&mut self, id: StreamId) -> Result<(), ConnectionError> {
        self.inner.close_stream_remote_half(id)
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

impl<T: ControlPing> ControlPing for StreamSendClose<T> {
    fn start_ping(&mut self, body: PingPayload) -> StartSend<PingPayload, ConnectionError> {
        self.inner.start_ping(body)
    }

    fn take_pong(&mut self) -> Option<PingPayload> {
        self.inner.take_pong()
    }
}
