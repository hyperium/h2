use {error, ConnectionError, FrameSize};
use frame::{self, Frame};
use proto::*;

use std::collections::VecDeque;

/// Tracks remote flow control windows.
#[derive(Debug)]
pub struct FlowControlSend<T>  {
    inner: T,

    initial_window_size: WindowSize,

    /// Tracks the onnection-level flow control window for receiving data from the remote.
    connection: FlowControlState,

    /// Holds the list of streams on which local window updates may be sent.
    // XXX It would be cool if this didn't exist.
    pending_streams: VecDeque<StreamId>,

    /// When `poll_window_update` is not ready, then the calling task is saved to
    /// be notified later. Access to poll_window_update must not be shared across tasks,
    /// as we only track a single task (and *not* i.e. a task per stream id).
    blocked: Option<task::Task>,
}

impl<T, U> FlowControlSend<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams
{
    pub fn new(initial_window_size: WindowSize, inner: T) -> FlowControlSend<T> {
        FlowControlSend {
            inner,
            initial_window_size,
            connection: FlowControlState::with_initial_size(initial_window_size),
            pending_streams: VecDeque::new(),
            blocked: None,
        }
    }
}

/// Exposes a public upward API for flow control.
impl<T: ControlStreams> ControlFlowSend for FlowControlSend<T> {
    fn poll_window_update(&mut self) -> Poll<WindowUpdate, ConnectionError> {
        // This biases connection window updates, which probably makes sense.
        if let Some(incr) = self.connection.apply_window_update() {
            return Ok(Async::Ready(WindowUpdate::new(StreamId::zero(), incr)));
        }

        // TODO this should probably account for stream priority?
        while let Some(id) = self.pending_streams.pop_front() {
            if let Some(mut flow) = self.send_flow_controller(id) {
                if let Some(incr) = flow.apply_window_update() {
                    return Ok(Async::Ready(WindowUpdate::new(id, incr)));
                }
            }
        }

        self.blocked = Some(task::current());
        return Ok(Async::NotReady);
    }
}

/// Applies remote window updates as they are received.
impl<T> Stream for FlowControlSend<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: ControlStreams,
{
    type Item = T::Item;
    type Error = T::Error;

    fn poll(&mut self) -> Poll<Option<T::Item>, T::Error> {
        trace!("poll");

        loop {
            match try_ready!(self.inner.poll()) {
                Some(Frame::WindowUpdate(v)) => {
                    let id = v.stream_id();
                    let sz = v.size_increment();

                    if id.is_zero() {
                        self.connection.expand_window(sz);
                    } else {
                        // The remote may send window updates for streams that the local
                        // now considers closed. It's okay.
                        if let Some(fc) = self.inner.send_flow_controller(id) {
                            fc.expand_window(sz);
                        }
                    }
                }

                f => return Ok(Async::Ready(f)),
            }
        }
    }
}

/// Tracks the flow control windows for sent davta frames.
///
/// If sending a frame would violate the remote's window, start_send fails with
/// `FlowControlViolation`.
impl<T, U> Sink for FlowControlSend<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ReadySink,
          T: ControlStreams,
          U: Buf,
 {
    type SinkItem = T::SinkItem;
    type SinkError = T::SinkError;

    fn start_send(&mut self, frame: Frame<U>) -> StartSend<T::SinkItem, T::SinkError> {
        debug_assert!(self.inner.get_reset(frame.stream_id()).is_none());

        // Ensures that the underlying transport is will accept the frame. It's important
        //  that this be checked before claiming capacity from the flow controllers.
        if self.poll_ready()?.is_not_ready() {
            return Ok(AsyncSink::NotReady(frame));
        }

        // Ensure that an outbound data frame does not violate the remote's flow control
        // window.
        if let &Frame::Data(ref v) = &frame {
            let sz = v.payload().remaining() as FrameSize;

            // Ensure there's enough capacity on the connection before acting on the
            // stream.
            if !self.connection.check_window(sz) {
                return Err(error::User::FlowControlViolation.into());
            }

            // Ensure there's enough capacity on stream.
            let mut fc = self.inner.send_flow_controller(v.stream_id())
                .expect("no remote stream for data frame");
            if fc.claim_window(sz).is_err() {
                return Err(error::User::FlowControlViolation.into())
            }

            self.connection.claim_window(sz)
                .expect("remote connection flow control error");
        }

        let res = self.inner.start_send(frame)?;
        assert!(res.is_ready());
        Ok(res)
    }

    fn poll_complete(&mut self) -> Poll<(), T::SinkError> {
        self.inner.poll_complete()
    }
}

/// Proxy.
impl<T, U> ReadySink for FlowControlSend<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ReadySink,
          T: ControlStreams,
          U: Buf,
{
    fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        self.inner.poll_ready()
    }
}

/// Applies an update to the remote endpoint's initial window size.
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
impl<T> ApplySettings for FlowControlSend<T>
    where T: ApplySettings,
          T: ControlStreams
{
    fn apply_local_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_local_settings(set)
    }

    fn apply_remote_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_remote_settings(set)?;

        if let Some(new_window_size) = set.initial_window_size() {
            let old_window_size = self.initial_window_size;
            if new_window_size == old_window_size {
                return Ok(());
            }

            self.inner.update_inital_send_window_size(old_window_size, new_window_size);
            self.initial_window_size = new_window_size;
        }

        Ok(())
    }
}

proxy_control_flow_recv!(FlowControlSend);
proxy_control_ping!(FlowControlSend);
proxy_control_streams!(FlowControlSend);
