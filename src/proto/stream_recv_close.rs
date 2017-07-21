use {ConnectionError};
use error::Reason;
use frame::{self, Frame};
use proto::*;
use proto::ready::ReadySink;

// TODO track "last stream id" for GOAWAY.
// TODO track/provide "next" stream id.
// TODO reset_streams needs to be bounded.
// TODO track reserved streams (PUSH_PROMISE).

#[derive(Debug)]
pub struct StreamRecvClose<T> {
    inner: T,
}

impl<T, U> StreamRecvClose<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams,
{
    pub fn new(inner: T) -> StreamRecvClose<T> {
        StreamRecvClose { inner }
    }
}

impl<T> Stream for StreamRecvClose<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: ControlStreams,
{
    type Item = T::Item;
    type Error = T::Error;

    fn poll(&mut self) -> Poll<Option<T::Item>, T::Error> {
        use frame::Frame::*;

        let frame = try_ready!(self.inner.poll());

        unimplemented!()
    }
}

impl<T, U> Sink for StreamRecvClose<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams,
{
    type SinkItem = Frame<U>;
    type SinkError = ConnectionError;

    fn start_send(&mut self, item: Self::SinkItem) -> StartSend<Frame<U>, ConnectionError> {
        self.inner.start_send(item)
    }

    fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
        self.inner.poll_complete()
    }
}

impl<T, U> ReadySink for StreamRecvClose<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ReadySink,
          T: ControlStreams,
{
    fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        self.inner.poll_ready()
    }
}

impl<T: ControlStreams> ControlStreams for StreamRecvClose<T> {
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

impl<T: ApplySettings> ApplySettings for StreamRecvClose<T> {
    fn apply_local_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_local_settings(set)
    }

    fn apply_remote_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_remote_settings(set)
    }
}

impl<T: ControlFlow> ControlFlow for StreamRecvClose<T> {
    fn poll_window_update(&mut self) -> Poll<WindowUpdate, ConnectionError> {
        self.inner.poll_window_update()
    }

    fn expand_window(&mut self, id: StreamId, incr: WindowSize) -> Result<(), ConnectionError> {
        self.inner.expand_window(id, incr)
    }
}

impl<T: ControlPing> ControlPing for StreamRecvClose<T> {
    fn start_ping(&mut self, body: PingPayload) -> StartSend<PingPayload, ConnectionError> {
        self.inner.start_ping(body)
    }

    fn take_pong(&mut self) -> Option<PingPayload> {
        self.inner.take_pong()
    }
}
