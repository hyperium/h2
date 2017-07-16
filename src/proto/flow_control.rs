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
    local_flow_controller: FlowControlState,

    /// Tracks the onnection-level flow control window for receiving data from the remote.
    remote_flow_controller: FlowControlState,

    /// Holds the list of streams on which local window updates may be sent.
    // XXX It would be cool if this didn't exist.
    pending_local_window_updates: VecDeque<StreamId>,

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
            local_flow_controller: FlowControlState::with_initial_size(initial_local_window_size),
            remote_flow_controller: FlowControlState::with_next_update(initial_remote_window_size),
            blocked_remote_window_update: None,
            sending_local_window_update: None,
            pending_local_window_updates: VecDeque::new(),
        }
    }
}

// Flow control utitlities.
impl<T: ControlStreams> FlowControl<T> {
    fn claim_local_window(&mut self, id: &StreamId, len: WindowSize) -> Result<(), ConnectionError> {
        let res = if id.is_zero() {
            self.local_flow_controller.claim_window(len)
        } else if let Some(mut stream) = self.inner.streams_mut().get_mut(&id) {
            stream.claim_local_window(len)
        } else {
            // Ignore updates for non-existent streams.
            Ok(())
        };

        res.map_err(|_| error::Reason::FlowControlError.into())
    }

    fn claim_remote_window(&mut self, id: &StreamId, len: WindowSize) -> Result<(), ConnectionError> {
        let res = if id.is_zero() {
            self.local_flow_controller.claim_window(len)
        } else if let Some(mut stream) = self.inner.streams_mut().get_mut(&id) {
            stream.claim_remote_window(len)
        } else {
            // Ignore updates for non-existent streams.
            Ok(())
        };

        res.map_err(|_| error::Reason::FlowControlError.into())
    }

    /// Handles a window update received from the remote, indicating that the local may
    /// send `incr` additional bytes.
    ///
    /// Connection window updates (id=0) and stream window updates are advertised
    /// distinctly.
    fn grow_remote_window(&mut self, id: StreamId, incr: WindowSize) {
        if incr == 0 {
            return;
        }

        if id.is_zero() {
            self.remote_flow_controller.grow_window(incr);
        } else if let Some(mut s) = self.inner.streams_mut().get_mut(&id) {
            s.grow_remote_window(incr);
        } else {
            // Ignore updates for non-existent streams.
            return;
        };

        if let Some(task) = self.blocked_remote_window_update.take() {
            task.notify();
        }
    }
}

/// Exposes a public upward API for flow control.
impl<T: ControlStreams> ControlFlow for FlowControl<T> {
    fn poll_remote_window_update(&mut self, id: StreamId) -> Poll<WindowSize, ConnectionError> {
        if id.is_zero() {
            if let Some(sz) = self.remote_flow_controller.take_window_update() {
                return Ok(Async::Ready(sz));
            }
        } else if let Some(mut stream) = self.inner.streams_mut().get_mut(&id) {
            if let Some(sz) = stream.take_remote_window_update() {
                return Ok(Async::Ready(sz));
            }
        } else {
            return Err(error::User::InvalidStreamId.into());
        }

        self.blocked_remote_window_update = Some(task::current());
        return Ok(Async::NotReady);
    }

    fn grow_local_window(&mut self, id: StreamId, incr: WindowSize) -> Result<(), ConnectionError> {
        if id.is_zero() {
            self.local_flow_controller.grow_window(incr);
            self.pending_local_window_updates.push_back(id);
            Ok(())
        } else if let Some(mut stream) = self.inner.streams_mut().get_mut(&id) {
            stream.grow_local_window(incr);
            self.pending_local_window_updates.push_back(id);
            Ok(())
        } else {
            Err(error::User::InvalidStreamId.into())
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

impl<T, U> FlowControl<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams,
{
    /// Returns ready when there are no pending window updates to send.
    fn poll_send_local_window_updates(&mut self) -> Poll<(), ConnectionError> {
        if let Some(f) = self.sending_local_window_update.take() {
            if self.inner.start_send(f.into())?.is_not_ready() {
                self.sending_local_window_update = Some(f);
                return Ok(Async::NotReady);
            }
        }

        while let Some(id) = self.pending_local_window_updates.pop_front() {
            let update = self.inner.streams_mut().get_mut(&id)
                .and_then(|mut s| s.take_local_window_update())
                .map(|incr| frame::WindowUpdate::new(id, incr));

            if let Some(f) = update {
                if self.inner.start_send(f.into())?.is_not_ready() {
                    self.sending_local_window_update = Some(f);
                    return Ok(Async::NotReady);
                }
            }
        }

        Ok(Async::Ready(()))
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
            streams.grow_all_local_windows(incr);
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
            streams.grow_all_remote_windows(incr);
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
                    self.grow_remote_window(v.stream_id(), v.size_increment());
                }

                Some(Data(v)) => {
                    self.claim_local_window(&v.stream_id(), v.len())?;
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

                self.claim_remote_window(&v.stream_id(), v.len())?;

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
