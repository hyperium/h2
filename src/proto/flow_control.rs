use ConnectionError;
use error;
use frame::{self, Frame};
use proto::*;

use futures::*;

#[derive(Debug)]
pub struct FlowControl<T>  {
    inner: T,
    initial_local_window_size: u32,
    initial_remote_window_size: u32,

    /// Tracks the connection-level flow control window for receiving data from the
    /// remote.
    local_flow_controller: FlowController,

    /// Tracks the onnection-level flow control window for receiving data from the remote.
    remote_flow_controller: FlowController,

    /// When `poll_window_update` is not ready, then the calling task is saved to be
    /// notified later. Access to poll_window_update must not be shared across tasks.
    blocked_window_update: Option<task::Task>,

    sending_window_update: Option<frame::WindowUpdate>,
}

impl<T, U> FlowControl<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: StreamTransporter
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
            local_flow_controller: FlowController::new(initial_local_window_size),
            remote_flow_controller: FlowController::new(initial_remote_window_size),
            blocked_window_update: None,
            sending_window_update: None,
        }
    }
}

impl<T: StreamTransporter> FlowControl<T> {
    fn claim_local_window(&mut self, id: &StreamId, len: WindowSize) -> Result<(), ConnectionError> {
        if id.is_zero() {
            return self.local_flow_controller.claim_window(len) 
                .map_err(|_| error::Reason::FlowControlError.into());
        }

        if let Some(mut stream) = self.streams_mut().get_mut(&id) {
            return stream.claim_local_window(len)
                .map_err(|_| error::Reason::FlowControlError.into());
        }

        // Ignore updates for non-existent streams.
        Ok(())
    }

    fn claim_remote_window(&mut self, id: &StreamId, len: WindowSize) -> Result<(), ConnectionError> {
        if id.is_zero() {
            return self.local_flow_controller.claim_window(len) 
                .map_err(|_| error::Reason::FlowControlError.into());
        }

        if let Some(mut stream) = self.streams_mut().get_mut(&id) {
            return stream.claim_remote_window(len)
                .map_err(|_| error::Reason::FlowControlError.into());
        }

        // Ignore updates for non-existent streams.
        Ok(())
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
        let added = if id.is_zero() {
            self.remote_flow_controller.grow_window(incr);
            true
        } else if let Some(mut s) = self.streams_mut().get_mut(&id) {
            s.grow_remote_window(incr);
            true
        } else {
            false
        };
        if added {
            if let Some(task) = self.blocked_window_update.take() {
                task.notify();
            }
        }
    }
}

impl<T, U> FlowControl<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
{
    /// Attempts to send a window update to the remote, if one is pending.
    fn poll_sending_window_update(&mut self) -> Poll<(), ConnectionError> {
        if let Some(f) = self.sending_window_update.take() {
            if self.inner.start_send(f.into())?.is_not_ready() {
                self.sending_window_update = Some(f);
                return Ok(Async::NotReady);
            }
        }

        Ok(Async::Ready(()))
    }
}

/// Applies an update to an endpoint's initial window size.
///
/// Per RFC 7540 §6.9.2
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
impl<T> ConnectionTransporter for FlowControl<T> 
    where T: ConnectionTransporter,
          T: StreamTransporter
{
    fn apply_local_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_local_settings(set)?;

        let old_window_size = self.initial_local_window_size;
        let new_window_size = set.initial_window_size();
        if new_window_size == old_window_size {
            return Ok(());
        }

        {
            let mut streams = self.streams_mut();
            if new_window_size < old_window_size {
                let decr = old_window_size - new_window_size;
                streams.shrink_all_local_windows(decr);
            } else { 
                let incr = new_window_size - old_window_size;
                streams.grow_all_local_windows(incr);
            }
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

        {
            let mut streams = self.streams_mut();
            if new_window_size < old_window_size {
                let decr = old_window_size - new_window_size;
                streams.shrink_all_remote_windows(decr);
            } else { 
                let incr = new_window_size - old_window_size;
                streams.grow_all_remote_windows(incr);
            }
        }
        
        self.initial_remote_window_size = new_window_size;
        Ok(())
    }
}

impl<T: StreamTransporter> StreamTransporter for FlowControl<T> {
    fn streams(&self) -> &StreamMap {
        self.inner.streams()
    }

    fn streams_mut(&mut self) -> &mut StreamMap {
        self.inner.streams_mut()
    }
}

impl<T> Stream for FlowControl<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: StreamTransporter,
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
          T: StreamTransporter,
 {
    type SinkItem = T::SinkItem;
    type SinkError = T::SinkError;

    fn start_send(&mut self, item: Frame<U>) -> StartSend<T::SinkItem, T::SinkError> {
        use frame::Frame::*;

        if let &Data(ref v) = &item {
            self.claim_remote_window(&v.stream_id(), v.len())?;
        }

        self.inner.start_send(item)
    }

    fn poll_complete(&mut self) -> Poll<(), T::SinkError> {
        self.inner.poll_complete()
    }
}

impl<T, U> ReadySink for FlowControl<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ReadySink,
          T: StreamTransporter,
{
    fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        self.inner.poll_ready()
    }
}
