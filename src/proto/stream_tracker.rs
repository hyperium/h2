use {ConnectionError};
use error::Reason;
use error::User;
use frame::{self, Frame};
use proto::*;

use fnv::FnvHasher;
use ordermap::OrderMap;
use std::hash::BuildHasherDefault;
use std::marker::PhantomData;

#[derive(Debug)]
pub struct StreamTracker<T, P> {
    inner: T,
    peer: PhantomData<P>,
    active_streams: StreamMap,
    // TODO reserved_streams: HashSet<StreamId>
    reset_streams: OrderMap<StreamId, Reason, BuildHasherDefault<FnvHasher>>,
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
            active_streams: StreamMap::default(),
            reset_streams: OrderMap::default(),
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
        &self.active_streams
    }

    #[inline]
    fn streams_mut(&mut self) -> &mut StreamMap {
        &mut self.active_streams
    }

    fn stream_is_reset(&self, id: StreamId) -> bool {
        self.reset_streams.contains_key(&id)
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

        loop {
            match try_ready!(self.inner.poll()) {
                Some(Headers(v)) => {
                    let id = v.stream_id();
                    let eos = v.is_end_stream();

                    if self.reset_streams.contains_key(&id) {
                        continue;
                    }

                    let is_closed = {
                        let stream = self.active_streams.entry(id)
                            .or_insert_with(|| StreamState::default());

                        let initialized =
                            stream.recv_headers::<P>(eos, self.initial_local_window_size)?;

                        if initialized {
                            // TODO: Ensure available capacity for a new stream
                            // This won't be as simple as self.streams.len() as closed
                            // connections should not be factored.

                            if !P::is_valid_remote_stream_id(id) {
                                return Err(Reason::ProtocolError.into());
                            }
                        }

                        stream.is_closed()
                    };

                    if is_closed {
                        self.active_streams.remove(&id);
                        self.reset_streams.insert(id, Reason::NoError);
                    }

                    return Ok(Async::Ready(Some(Headers(v))));
                }

                Some(Data(v)) => {
                    let id = v.stream_id();

                    if self.reset_streams.contains_key(&id) {
                        continue;
                    }

                    let is_closed = {
                        let stream = match self.active_streams.get_mut(&id) {
                            None => return Err(Reason::ProtocolError.into()),
                            Some(s) => s,
                        };
                        stream.recv_data(v.is_end_stream())?;
                        stream.is_closed()
                    };

                    if is_closed {
                        self.active_streams.remove(&id);
                        self.reset_streams.insert(id, Reason::NoError);
                    }

                    return Ok(Async::Ready(Some(Data(v))));
                }

                Some(Reset(v)) => {
                    let id = v.stream_id();

                    // Set or update the reset reason.
                    self.reset_streams.insert(id, v.reason());

                    if self.active_streams.remove(&id).is_some() {
                        return Ok(Async::Ready(Some(Reset(v))));
                    }
                }

                Some(f) => {
                    let id = f.stream_id();

                    if self.reset_streams.contains_key(&id) {
                        continue;
                    }

                    return Ok(Async::Ready(Some(f)));
                }

                None => {
                    return Ok(Async::Ready(None));
                }
            }
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

        // Must be enforced through higher levels.
        debug_assert!(!self.stream_is_reset(item.stream_id()));

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

                let is_closed = {
                    let stream = self.active_streams.entry(id)
                        .or_insert_with(|| StreamState::default());

                    let initialized =
                        stream.send_headers::<P>(eos, self.initial_remote_window_size)?;

                    if initialized {
                        // TODO: Ensure available capacity for a new stream
                        // This won't be as simple as self.streams.len() as closed
                        // connections should not be factored.
                        if !P::is_valid_local_stream_id(id) {
                            // TODO: clear state
                            return Err(User::InvalidStreamId.into());
                        }
                    }

                    stream.is_closed()
                };

                if is_closed {
                    self.active_streams.remove(&id);
                    self.reset_streams.insert(id, Reason::NoError);
                }
            }

            &Data(ref v) => {
                match self.active_streams.get_mut(&v.stream_id()) {
                    None => return Err(User::InactiveStreamId.into()),
                    Some(stream) => stream.send_data(v.is_end_stream())?,
                }
            }


            &Reset(ref v) => {
                let id = v.stream_id();
                self.active_streams.remove(&id);
                self.reset_streams.insert(id, v.reason());
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
