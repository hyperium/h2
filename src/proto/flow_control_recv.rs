use {error, ConnectionError, FrameSize};
use frame::{self, Frame};
use proto::*;

use std::collections::VecDeque;

/// Tracks local flow control windows.
#[derive(Debug)]
pub struct FlowControlRecv<T>  {
    inner: T,


    initial_window_size: WindowSize,

    /// Tracks the connection-level flow control window for receiving data from the
    /// remote.
    connection: FlowControlState,

    /// Holds the list of streams on which local window updates may be sent.
    // XXX It would be cool if this didn't exist.
    pending_streams: VecDeque<StreamId>,

    /// If a window update can't be sent immediately, it may need to be saved to be sent
    /// later.
    sending: Option<frame::WindowUpdate>,
}

impl<T, U> FlowControlRecv<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams
{
    pub fn new(initial_window_size: WindowSize, inner: T) -> FlowControlRecv<T> {
        FlowControlRecv {
            inner,
            initial_window_size,
            connection: FlowControlState::with_initial_size(initial_window_size),
            pending_streams: VecDeque::new(),
            sending: None,
        }
    }
}

/// Exposes a public upward API for flow control.
impl<T: ControlStreams> ControlFlowRecv for FlowControlRecv<T> {
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
                self.pending_streams.push_back(id);
            }
            Ok(())
        } else if let Some(rst) = self.inner.get_reset(id) {
            Err(error::User::StreamReset(rst).into())
        } else {
            Err(error::User::InvalidStreamId.into())
        }
    }
}

impl<T, U> FlowControlRecv<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams,
{
    /// Returns ready when there are no pending window updates to send.
    fn poll_send_local(&mut self) -> Poll<(), ConnectionError> {
        if let Some(f) = self.sending.take() {
            try_ready!(self.try_send(f));
        }

        if let Some(incr) = self.connection.apply_window_update() {
            try_ready!(self.try_send(frame::WindowUpdate::new(StreamId::zero(), incr)));
        }

        while let Some(id) = self.pending_streams.pop_front() {
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
            self.sending = Some(f);
            Ok(Async::NotReady)
        } else {
            Ok(Async::Ready(()))
        }
    }
}

/// Ensures that the remote does not violate the local peer's flow controller.
impl<T> Stream for FlowControlRecv<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: ControlStreams,
{
    type Item = T::Item;
    type Error = T::Error;

    fn poll(&mut self) -> Poll<Option<T::Item>, T::Error> {
        trace!("poll");
        loop {
            match try_ready!(self.inner.poll()) {
                Some(Frame::Data(v)) => {
                    let id = v.stream_id();
                    let sz = v.payload().len() as FrameSize;

                    // Ensure there's enough capacity on the connection before acting on
                    // the stream.
                    if !self.connection.check_window(sz) {
                        // TODO this should cause a GO_AWAY
                        return Err(error::Reason::FlowControlError.into());
                    }

                    let fc = self.inner.recv_flow_controller(id)
                        .expect("receiving data with no flow controller");
                    if fc.claim_window(sz).is_err() {
                        // TODO this should cause a GO_AWAY
                        return Err(error::Reason::FlowControlError.into());
                    }

                    self.connection.claim_window(sz)
                        .expect("local connection flow control error");

                    return Ok(Async::Ready(Some(Frame::Data(v))));
                }

                v => return Ok(Async::Ready(v)),
            }
        }
    }
}

/// Sends pending window updates before operating on the underlying transport.
impl<T, U> Sink for FlowControlRecv<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ReadySink,
          T: ControlStreams,
 {
    type SinkItem = T::SinkItem;
    type SinkError = T::SinkError;

    fn start_send(&mut self, frame: Frame<U>) -> StartSend<T::SinkItem, T::SinkError> {
        if self.poll_send_local()?.is_not_ready() {
            return Ok(AsyncSink::NotReady(frame));
        }
        self.inner.start_send(frame)
    }

    fn poll_complete(&mut self) -> Poll<(), T::SinkError> {
        try_ready!(self.poll_send_local());
        self.inner.poll_complete()
    }
}

/// Sends pending window updates before checking the underyling transport's readiness.
impl<T, U> ReadySink for FlowControlRecv<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ReadySink,
          T: ControlStreams,
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
impl<T> ApplySettings for FlowControlRecv<T>
    where T: ApplySettings,
          T: ControlStreams
{
    fn apply_local_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_local_settings(set)?;

        if let Some(new_window_size) = set.initial_window_size() {
            let old_window_size = self.initial_window_size;
            if new_window_size == old_window_size {
                return Ok(());
            }

            self.inner.update_inital_recv_window_size(old_window_size, new_window_size);
            self.initial_window_size = new_window_size;
        }
        Ok(())
    }

    fn apply_remote_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_remote_settings(set)
    }
}

proxy_control_flow_send!(FlowControlRecv);
proxy_control_ping!(FlowControlRecv);
proxy_control_streams!(FlowControlRecv);
