use ConnectionError;
use frame::{self, Frame};
use proto::{ReadySink, StreamMap, ConnectionTransporter, StreamTransporter};

use futures::*;

#[derive(Debug)]
pub struct StreamTracker<T> {
    inner: T,
    streams: StreamMap,
    local_max_concurrency: Option<u32>,
    remote_max_concurrency: Option<u32>,
}

impl<T, U> StreamTracker<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>
{
    pub fn new(local_max_concurrency: Option<u32>,
               remote_max_concurrency: Option<u32>,
               inner: T)
        -> StreamTracker<T>
    {
        StreamTracker {
            inner,
            streams: StreamMap::default(),
            local_max_concurrency,
            remote_max_concurrency,
        }
    }
}

impl<T> StreamTransporter for StreamTracker<T> {
    fn streams(&self) -> &StreamMap {
        &self.streams
    }

    fn streams_mut(&mut self) -> &mut StreamMap {
        &mut self.streams
    }
}

/// Handles updates to `SETTINGS_MAX_CONCURRENT_STREAMS`.
///
/// > Indicates the maximum number of concurrent streams that the sender will allow. This
/// > limit is directional: it applies to the number of streams that the sender permits the
/// > receiver to create. Initially, there is no limit to this value. It is recommended that
/// > this value be no smaller than 100, so as to not unnecessarily limit parallelism.
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
impl<T: ConnectionTransporter> ConnectionTransporter for StreamTracker<T> {
    fn apply_local_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.local_max_concurrency = set.max_concurrent_streams();
        self.inner.apply_local_settings(set)
    }

    fn apply_remote_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.remote_max_concurrency = set.max_concurrent_streams();
        self.inner.apply_remote_settings(set)
    }
}

impl<T, U> Stream for StreamTracker<T>
    where T: Stream<Item = Frame<U>, Error = ConnectionError>,
{
    type Item = T::Item;
    type Error = T::Error;

    fn poll(&mut self) -> Poll<Option<T::Item>, T::Error> {
        self.inner.poll()
    }
}


impl<T, U> Sink for StreamTracker<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
{
    type SinkItem = T::SinkItem;
    type SinkError = T::SinkError;

    fn start_send(&mut self, item: T::SinkItem) -> StartSend<T::SinkItem, T::SinkError> {
        self.inner.start_send(item)
    }

    fn poll_complete(&mut self) -> Poll<(), T::SinkError> {
        self.inner.poll_complete()
    }
}


impl<T, U> ReadySink for StreamTracker<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ReadySink,
{
    fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        self.inner.poll_ready()
    }
}
