use ConnectionError;
use error;
use frame::{self, Frame};
use proto::*;

use std::collections::VecDeque;

#[derive(Debug)]
pub struct FlowControl<T>  {
    inner: T,
    initial_local_window_size: u32,
    initial_remote_window_size: u32,

    /// Tracks the connection-level flow control window for receiving data from the
    /// remote.
    connection_local_flow_controller: FlowControlState,

    /// Tracks the onnection-level flow control window for receiving data from the remote.
    connection_remote_flow_controller: FlowControlState,

    /// Holds the list of streams on which local window updates may be sent.
    // XXX It would be cool if this didn't exist.
    pending_local_window_updates: VecDeque<StreamId>,
    pending_local_connection_window_update: bool,

    /// If a window update can't be sent immediately, it may need to be saved to be sent later.
    sending_local_window_update: Option<frame::WindowUpdate>,

    /// When `poll_remote_window_update` is not ready, then the calling task is saved to
    /// be notified later. Access to poll_window_update must not be shared across tasks,
    /// as we only track a single task (and *not* i.e. a task per stream id).
    blocked_remote_window_update: Option<task::Task>,
}

impl<T, U> FlowControl<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams
{
    pub fn new(initial_local_window_size: u32,
               initial_remote_window_size: u32,
               inner: T)
        -> FlowControl<T>
    {
        FlowControl {
            inner,
            initial_local_window_size,
            initial_remote_window_size,
            connection_local_flow_controller: FlowControlState::with_initial_size(initial_local_window_size),
            connection_remote_flow_controller: FlowControlState::with_initial_size(initial_remote_window_size),
            blocked_remote_window_update: None,
            sending_local_window_update: None,
            pending_local_window_updates: VecDeque::new(),
            pending_local_connection_window_update: false,
        }
    }
}

// Flow control utitlities.
impl<T: ControlStreams> FlowControl<T> {
    fn local_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState> {
        if id.is_zero() {
            Some(&mut self.connection_local_flow_controller)
        } else {
            self.inner.streams_mut().get_mut(&id).and_then(|s| s.local_flow_controller())
        }
    }

   fn remote_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState> {
        if id.is_zero() {
            Some(&mut self.connection_remote_flow_controller)
        } else {
            self.inner.streams_mut().get_mut(&id).and_then(|s| s.remote_flow_controller())
        }
    }
}

/// Proxies access to streams.
impl<T: ControlStreams> ControlStreams for FlowControl<T> {
    #[inline]
    fn streams(&self) -> &StreamMap {
        self.inner.streams()
    }

    #[inline]
    fn streams_mut(&mut self) -> &mut StreamMap {
        self.inner.streams_mut()
    }
}

/// Exposes a public upward API for flow control.
impl<T: ControlStreams> ControlFlow for FlowControl<T> {
    fn poll_remote_window_update(&mut self, id: StreamId) -> Poll<WindowSize, ConnectionError> {
        if let Some(mut flow) = self.remote_flow_controller(id) {
            if let Some(sz) = flow.apply_window_update() {
                return Ok(Async::Ready(sz));
            }
        } else {
            return Err(error::User::InvalidStreamId.into());
        }

        self.blocked_remote_window_update = Some(task::current());
        return Ok(Async::NotReady);
    }

    fn expand_local_window(&mut self, id: StreamId, incr: WindowSize) -> Result<(), ConnectionError> {
        if let Some(mut fc) = self.local_flow_controller(id) {
            fc.expand_window(incr);
        } else {
            return Err(error::User::InvalidStreamId.into());
        }

        if id.is_zero() {
            self.pending_local_connection_window_update = true;
        } else {
            self.pending_local_window_updates.push_back(id);
        }
        Ok(())
    }
}

impl<T: ControlPing> ControlPing for FlowControl<T> {
    fn start_ping(&mut self, body: PingPayload) -> StartSend<PingPayload, ConnectionError> {
        self.inner.start_ping(body)
    }

    fn pop_pong(&mut self) -> Option<PingPayload> {
        self.inner.pop_pong()
    }
}

impl<T, U> FlowControl<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams,
{
    /// Returns ready when there are no pending window updates to send.
    fn poll_send_local_window_updates(&mut self) -> Poll<(), ConnectionError> {
        if let Some(f) = self.sending_local_window_update.take() {
            try_ready!(self.try_send(f));
        }

        if self.pending_local_connection_window_update {
            if let Some(incr) = self.connection_local_flow_controller.apply_window_update() {
                try_ready!(self.try_send(frame::WindowUpdate::new(StreamId::zero(), incr)));
            }
        }

        while let Some(id) = self.pending_local_window_updates.pop_front() {
            let update = self.local_flow_controller(id).and_then(|s| s.apply_window_update());
            if let Some(incr) = update {
                try_ready!(self.try_send(frame::WindowUpdate::new(id, incr)));
            }
        }

        Ok(Async::Ready(()))
    }

    fn try_send(&mut self, f: frame::WindowUpdate) -> Poll<(), ConnectionError> {
        if self.inner.start_send(f.into())?.is_not_ready() {
            self.sending_local_window_update = Some(f);
            Ok(Async::NotReady)
        } else {
            Ok(Async::Ready(()))
        }
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

        let old_window_size = self.initial_local_window_size;
        let new_window_size = set.initial_window_size();
        if new_window_size == old_window_size {
            return Ok(());
        }

        let mut streams = self.inner.streams_mut();
        if new_window_size < old_window_size {
            let decr = old_window_size - new_window_size;
            streams.shrink_all_local_windows(decr);
        } else { 
            let incr = new_window_size - old_window_size;
            streams.expand_all_local_windows(incr);
        }
        
        self.initial_local_window_size = new_window_size;
        Ok(())
    }

    fn apply_remote_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_remote_settings(set)?;

        let old_window_size = self.initial_remote_window_size;
        let new_window_size = set.initial_window_size();
        if new_window_size == old_window_size {
            return Ok(());
        }

        let mut streams = self.inner.streams_mut();
        if new_window_size < old_window_size {
            let decr = old_window_size - new_window_size;
            streams.shrink_all_remote_windows(decr);
        } else { 
            let incr = new_window_size - old_window_size;
            streams.expand_all_remote_windows(incr);
        }
        
        self.initial_remote_window_size = new_window_size;
        Ok(())
    }
}

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
                    if let Some(fc) = self.remote_flow_controller(v.stream_id()) {
                        fc.expand_window(v.size_increment());
                    }
                }

                Some(Data(v)) => {
                    if self.connection_local_flow_controller.claim_window(v.len()).is_err() {
                        return Err(error::Reason::FlowControlError.into())
                    }
                    if let Some(fc) = self.local_flow_controller(v.stream_id()) {
                        if fc.claim_window(v.len()).is_err() {
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

impl<T, U> Sink for FlowControl<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ReadySink,
          T: ControlStreams,
 {
    type SinkItem = T::SinkItem;
    type SinkError = T::SinkError;

    fn start_send(&mut self, frame: Frame<U>) -> StartSend<T::SinkItem, T::SinkError> {
        use frame::Frame::*;

        if self.poll_send_local_window_updates()?.is_not_ready() {
            return Ok(AsyncSink::NotReady(frame));
        }

        match frame {
            Data(v) => {
                // Before claiming space, ensure that the transport will accept the frame.
                if self.inner.poll_ready()?.is_not_ready() {
                    return Ok(AsyncSink::NotReady(Data(v)));
                }

                if self.connection_remote_flow_controller.claim_window(v.len()).is_err() {
                    return Err(error::User::FlowControlViolation.into());
                }
                if let Some(fc) = self.remote_flow_controller(v.stream_id()) {
                    if fc.claim_window(v.len()).is_err() {
                        return Err(error::User::FlowControlViolation.into())
                    }
                }

                let res = self.inner.start_send(Data(v))?;
                assert!(res.is_ready());
                Ok(res)
            }

            frame => self.inner.start_send(frame),
        }
    }

    fn poll_complete(&mut self) -> Poll<(), T::SinkError> {
        try_ready!(self.inner.poll_complete());
        self.poll_send_local_window_updates()
    }
}

impl<T, U> ReadySink for FlowControl<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ReadySink,
          T: ControlStreams,
{
    fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        try_ready!(self.inner.poll_ready());
        self.poll_send_local_window_updates()
    }
}
