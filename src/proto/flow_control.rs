use ConnectionError;
use frame::{self, Frame};
use proto::{ReadySink, StreamMap, ConnectionTransporter, StreamTransporter};

use futures::*;

#[derive(Debug)]
pub struct FlowControl<T>  {
    inner: T,
    initial_local_window_size: u32,
    initial_remote_window_size: u32,
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
        }
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
                streams.shrink_local_window(decr);
            } else { 
                let incr = new_window_size - old_window_size;
                streams.grow_local_window(incr);
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
                streams.shrink_remote_window(decr);
            } else { 
                let incr = new_window_size - old_window_size;
                streams.grow_remote_window(incr);
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
        self.inner.poll()
    }
}


impl<T, U> Sink for FlowControl<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: StreamTransporter,
 {
    type SinkItem = T::SinkItem;
    type SinkError = T::SinkError;

    fn start_send(&mut self, item: Frame<U>) -> StartSend<T::SinkItem, T::SinkError> {
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
