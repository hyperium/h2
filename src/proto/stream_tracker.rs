use ConnectionError;
use error::Reason;
use error::User;
use frame::{self, Frame};
use proto::*;

use std::marker::PhantomData;

#[derive(Debug)]
pub struct StreamTracker<T, P> {
    inner: T,
    peer: PhantomData<P>,
    streams: StreamMap,
    local_max_concurrency: Option<u32>,
    remote_max_concurrency: Option<u32>,
    initial_local_window_size: WindowSize,
    initial_remote_window_size: WindowSize,
}

impl<T, P, U> StreamTracker<T, P>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          P: Peer
{
    pub fn new(initial_local_window_size: WindowSize,
               initial_remote_window_size: WindowSize,
               local_max_concurrency: Option<u32>,
               remote_max_concurrency: Option<u32>,
               inner: T)
        -> StreamTracker<T, P>
    {
        StreamTracker {
            inner,
            peer: PhantomData,
            streams: StreamMap::default(),
            local_max_concurrency,
            remote_max_concurrency,
            initial_local_window_size,
            initial_remote_window_size,
        }
    }
}

impl<T, P> ControlStreams for StreamTracker<T, P> {
    #[inline]
    fn streams(&self) -> &StreamMap {
        &self.streams
    }

    #[inline]
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
impl<T, P> ApplySettings for StreamTracker<T, P>
    where T: ApplySettings
{
    fn apply_local_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.local_max_concurrency = set.max_concurrent_streams();
        self.initial_local_window_size = set.initial_window_size();
        self.inner.apply_local_settings(set)
    }

    fn apply_remote_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.remote_max_concurrency = set.max_concurrent_streams();
        self.initial_remote_window_size = set.initial_window_size();
        self.inner.apply_remote_settings(set)
    }
}

impl<T, P> ControlPing for StreamTracker<T, P>
    where T: ControlPing
{
    fn start_ping(&mut self, body: PingPayload) -> StartSend<PingPayload, ConnectionError> {
        self.inner.start_ping(body)
    }

    fn pop_pong(&mut self) -> Option<PingPayload> {
        self.inner.pop_pong()
    }
}

impl<T, P> Stream for StreamTracker<T, P>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          P: Peer,
{
    type Item = T::Item;
    type Error = T::Error;

    fn poll(&mut self) -> Poll<Option<T::Item>, T::Error> {
        use frame::Frame::*;

        match try_ready!(self.inner.poll()) {
            Some(Headers(v)) => {
                let id = v.stream_id();
                let eos = v.is_end_stream();

                let initialized = self.streams
                    .entry(id)
                    .or_insert_with(|| StreamState::default())
                    .recv_headers::<P>(eos, self.initial_local_window_size)?;

                if initialized {
                    // TODO: Ensure available capacity for a new stream
                    // This won't be as simple as self.streams.len() as closed
                    // connections should not be factored.

                    if !P::is_valid_remote_stream_id(id) {
                        return Err(Reason::ProtocolError.into());
                    }
                }

                Ok(Async::Ready(Some(Headers(v))))
            }

            Some(Data(v)) => {
                match self.streams.get_mut(&v.stream_id()) {
                    None => Err(Reason::ProtocolError.into()),
                    Some(state) => {
                        state.recv_data(v.is_end_stream())?;
                        Ok(Async::Ready(Some(Data(v))))
                    }
                }
            }

            f => Ok(Async::Ready(f)),
        }
    }
}


impl<T, P, U> Sink for StreamTracker<T, P>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          P: Peer,
{
    type SinkItem = T::SinkItem;
    type SinkError = T::SinkError;

    fn start_send(&mut self, item: T::SinkItem) -> StartSend<T::SinkItem, T::SinkError> {
        use frame::Frame::*;

        match &item {
            &Headers(ref v) => {
                let id = v.stream_id();
                let eos = v.is_end_stream();

                // Transition the stream state, creating a new entry if needed
                //
                // TODO: Response can send multiple headers frames before body (1xx
                // responses).
                //
                // ACTUALLY(ver), maybe not?
                //   https://github.com/http2/http2-spec/commit/c83c8d911e6b6226269877e446a5cad8db921784
                let initialized = self.streams
                    .entry(id)
                    .or_insert_with(|| StreamState::default())
                    .send_headers::<P>(eos, self.initial_remote_window_size)?;

                if initialized {
                    // TODO: Ensure available capacity for a new stream
                    // This won't be as simple as self.streams.len() as closed
                    // connections should not be factored.
                    if !P::is_valid_local_stream_id(id) {
                        // TODO: clear state
                        return Err(User::InvalidStreamId.into());
                    }
                }
            }

            &Data(ref v) => {
                match self.streams.get_mut(&v.stream_id()) {
                    None => return Err(User::InactiveStreamId.into()),
                    Some(state) => state.send_data(v.is_end_stream())?,
                }
            }

            _ => {}
        }

        self.inner.start_send(item)

    }

    fn poll_complete(&mut self) -> Poll<(), T::SinkError> {
        self.inner.poll_complete()
    }
}


impl<T, P, U> ReadySink for StreamTracker<T, P>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ReadySink,
          P: Peer,
{
    fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        self.inner.poll_ready()
    }
}
