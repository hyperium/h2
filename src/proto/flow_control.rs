use {error, ConnectionError, FrameSize};
use frame::{self, Frame};
use proto::*;

use std::collections::VecDeque;

#[derive(Debug)]
pub struct FlowControl<T>  {
    inner: T,

    local_initial: WindowSize,
    remote_initial: WindowSize,

    /// Tracks the connection-level flow control window for receiving data from the
    /// remote.
    local_connection: FlowControlState,

    /// Tracks the onnection-level flow control window for receiving data from the remote.
    remote_connection: FlowControlState,

    /// Holds the list of streams on which local window updates may be sent.
    // XXX It would be cool if this didn't exist.
    local_pending_streams: VecDeque<StreamId>,

    /// If a window update can't be sent immediately, it may need to be saved to be sent
    /// later.
    local_sending: Option<frame::WindowUpdate>,

    /// Holds the list of streams on which local window updates may be sent.
    // XXX It would be cool if this didn't exist.
    remote_pending_streams: VecDeque<StreamId>,

    /// When `poll_window_update` is not ready, then the calling task is saved to
    /// be notified later. Access to poll_window_update must not be shared across tasks,
    /// as we only track a single task (and *not* i.e. a task per stream id).
    remote_blocked: Option<task::Task>,
}

impl<T, U> FlowControl<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams
{
    pub fn new(local_initial: WindowSize,
               remote_initial: WindowSize,
               inner: T)
        -> FlowControl<T>
    {
        FlowControl {
            inner,

            local_initial,
            local_connection: FlowControlState::with_initial_size(local_initial),
            local_sending: None,
            local_pending_streams: VecDeque::new(),

            remote_initial,
            remote_connection: FlowControlState::with_initial_size(remote_initial),
            remote_blocked: None,
            remote_pending_streams: VecDeque::new(),
        }
    }
}

// Flow control utitlities.
impl<T: ControlStreams> FlowControl<T> {
    fn recv_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState> {
        if id.is_zero() {
            Some(&mut self.local_connection)
        } else {
            self.inner.recv_flow_controller(id)
        }
    }

   fn send_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState> {
        if id.is_zero() {
            Some(&mut self.remote_connection)
        } else {
            self.inner.send_flow_controller(id)
        }
    }
}
/// Exposes a public upward API for flow control.
impl<T: ControlStreams> ControlFlow for FlowControl<T> {
    fn poll_window_update(&mut self) -> Poll<WindowUpdate, ConnectionError> {
        // This biases connection window updates, which probably makese sense.
        if let Some(incr) = self.remote_connection.apply_window_update() {
            return Ok(Async::Ready(WindowUpdate::new(StreamId::zero(), incr)));
        }

        // TODO this should probably account for stream priority?
        while let Some(id) = self.remote_pending_streams.pop_front() {
            if let Some(mut flow) = self.send_flow_controller(id) {
                if let Some(incr) = flow.apply_window_update() {
                    return Ok(Async::Ready(WindowUpdate::new(id, incr)));
                }
            }
        }

        self.remote_blocked = Some(task::current());
        return Ok(Async::NotReady);
    }

    fn expand_window(&mut self, id: StreamId, incr: WindowSize) -> Result<(), ConnectionError> {
        let added = match self.recv_flow_controller(id) {
            None => false,
            Some(mut fc) => {
                fc.expand_window(incr);
                true
            }
        };

        if added {
            if !id.is_zero() {
                self.local_pending_streams.push_back(id);
            }
            Ok(())
        } else if let Some(rst) = self.inner.get_reset(id) {
            Err(error::User::StreamReset(rst).into())
        } else {
            Err(error::User::InvalidStreamId.into())
        }
    }
}

impl<T, U> FlowControl<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams,
{
    /// Returns ready when there are no pending window updates to send.
    fn poll_send_local(&mut self) -> Poll<(), ConnectionError> {
        if let Some(f) = self.local_sending.take() {
            try_ready!(self.try_send(f));
        }

        if let Some(incr) = self.local_connection.apply_window_update() {
            try_ready!(self.try_send(frame::WindowUpdate::new(StreamId::zero(), incr)));
        }

        while let Some(id) = self.local_pending_streams.pop_front() {
            if self.inner.get_reset(id).is_none() {
                let update = self.recv_flow_controller(id).and_then(|s| s.apply_window_update());
                if let Some(incr) = update {
                    try_ready!(self.try_send(frame::WindowUpdate::new(id, incr)));
                }
            }
        }

        Ok(Async::Ready(()))
    }

    fn try_send(&mut self, f: frame::WindowUpdate) -> Poll<(), ConnectionError> {
        if self.inner.start_send(f.into())?.is_not_ready() {
            self.local_sending = Some(f);
            Ok(Async::NotReady)
        } else {
            Ok(Async::Ready(()))
        }
    }
}

/// Tracks window updates received from the remote and ensures that the remote does not
/// violate the local peer's flow controller.
///
/// TODO send flow control reset when the peer violates the flow control window.
impl<T> Stream for FlowControl<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: ControlStreams,
 {
    type Item = T::Item;
    type Error = T::Error;

    fn poll(&mut self) -> Poll<Option<T::Item>, T::Error> {
        use frame::Frame::*;
        trace!("poll");

        loop {
            match try_ready!(self.inner.poll()) {
                Some(WindowUpdate(v)) => {
                    if let Some(fc) = self.send_flow_controller(v.stream_id()) {
                        fc.expand_window(v.size_increment());
                    }
                }

                Some(Data(v)) => {
                    let sz = v.payload().len() as FrameSize;
                    if self.local_connection.claim_window(sz).is_err() {
                        return Err(error::Reason::FlowControlError.into())
                    }
                    // If this frame ends the stream, there may no longer be a flow
                    // controller.  That's fine.
                    if let Some(fc) = self.recv_flow_controller(v.stream_id()) {
                        if fc.claim_window(sz).is_err() {
                            // TODO send flow control reset.
                            return Err(error::Reason::FlowControlError.into())
                        }
                    }
                    return Ok(Async::Ready(Some(Data(v))));
                }

                v => return Ok(Async::Ready(v)),
            }
        }
    }
}

/// Tracks the send flow control windows for sent frames.
///
/// If sending a frame would violate the remote's window, start_send fails with
/// `FlowControlViolation`.
///
/// Sends pending window updates before operating on the underlying transport.
impl<T, U> Sink for FlowControl<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ReadySink,
          T: ControlStreams,
          U: Buf,
 {
    type SinkItem = T::SinkItem;
    type SinkError = T::SinkError;

    fn start_send(&mut self, frame: Frame<U>) -> StartSend<T::SinkItem, T::SinkError> {
        use frame::Frame::*;

        debug_assert!(self.inner.get_reset(frame.stream_id()).is_none());

        // Ensures that:
        // 1. all pending local window updates have been sent to the remote.
        // 2. the underlying transport is will accept the frame. It's important that this
        //    be checked before claiming capacity from the flow controllers.
        if self.poll_ready()?.is_not_ready() {
            return Ok(AsyncSink::NotReady(frame));
        }

        // Ensure that an outbound data frame does not violate the remote's flow control
        // window.
        if let &Data(ref v) = &frame {
            let sz = v.payload().remaining() as FrameSize;

            // Ensure there's enough capacity on the connection before acting on the
            // stream.
            if !self.remote_connection.check_window(sz) {
                return Err(error::User::FlowControlViolation.into());
            }

            // Ensure there's enough capacity on stream.
            {
                let mut fc = self.inner.send_flow_controller(v.stream_id())
                    .expect("no remote stream for data frame");
                if fc.claim_window(sz).is_err() {
                    return Err(error::User::FlowControlViolation.into())
                }
            }

            self.remote_connection.claim_window(sz)
                .expect("remote connection flow control error");
        }

        let res = self.inner.start_send(frame)?;
        assert!(res.is_ready());
        Ok(res)
    }

    fn poll_complete(&mut self) -> Poll<(), T::SinkError> {
        try_ready!(self.poll_send_local());
        self.inner.poll_complete()
    }
}

/// Sends pending window updates before checking the underyling transport's readiness.
impl<T, U> ReadySink for FlowControl<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ReadySink,
          T: ControlStreams,
          U: Buf,
{
    fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        try_ready!(self.poll_send_local());
        self.inner.poll_ready()
    }
}

/// Applies an update to an endpoint's initial window size.
///
/// Per RFC 7540 §6.9.2:
///
/// > In addition to changing the flow-control window for streams that are not yet
/// > active, a SETTINGS frame can alter the initial flow-control window size for
/// > streams with active flow-control windows (that is, streams in the "open" or
/// > "half-closed (remote)" state). When the value of SETTINGS_INITIAL_WINDOW_SIZE
/// > changes, a receiver MUST adjust the size of all stream flow-control windows that
/// > it maintains by the difference between the new value and the old value.
/// >
/// > A change to `SETTINGS_INITIAL_WINDOW_SIZE` can cause the available space in a
/// > flow-control window to become negative. A sender MUST track the negative
/// > flow-control window and MUST NOT send new flow-controlled frames until it
/// > receives WINDOW_UPDATE frames that cause the flow-control window to become
/// > positive.
impl<T> ApplySettings for FlowControl<T> 
    where T: ApplySettings,
          T: ControlStreams
{
    fn apply_local_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_local_settings(set)?;

        if let Some(new_window_size) = set.initial_window_size() {
            let old_window_size = self.local_initial;
            if new_window_size == old_window_size {
                return Ok(());
            }

            self.inner.update_inital_recv_window_size(old_window_size, new_window_size);
            self.local_initial = new_window_size;
        }
        Ok(())
    }

    fn apply_remote_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_remote_settings(set)?;

        if let Some(new_window_size) = set.initial_window_size() {
            let old_window_size = self.remote_initial;
            if new_window_size == old_window_size {
                return Ok(());
            }

            self.inner.update_inital_send_window_size(old_window_size, new_window_size);
            self.remote_initial = new_window_size;
        }
        Ok(())
    }
}

proxy_control_streams!(FlowControl);
proxy_control_ping!(FlowControl);
