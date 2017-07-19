use {ConnectionError};
use error::Reason;
use error::User;
use frame::{self, Frame};
use proto::*;

use fnv::FnvHasher;
use ordermap::OrderMap;
use std::hash::BuildHasherDefault;
use std::marker::PhantomData;

// TODO track "last stream id" for GOAWAY.
// TODO track/provide "next" stream id.
// TODO reset_streams needs to be bounded.
// TODO track reserved streams (PUSH_PROMISE).

/// Tracks a connection's streams.
#[derive(Debug)]
pub struct StreamSend<T, P> {
    inner: T,
    peer: PhantomData<P>,
    max_concurrency: Option<u32>,
    initial_window_size: WindowSize,
}

impl<T, P, U> StreamSend<T, P>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          P: Peer
{
    pub fn new(initial_window_size: WindowSize,
               max_concurrency: Option<u32>,
               inner: T)
        -> StreamSend<T, P>
    {
        StreamSend {
            inner,
            peer: PhantomData,
            max_concurrency,
            initial_window_size,
        }
    }

    pub fn try_open_local(&mut self, frame: Frame) -> Result<(), ConnectionError> {
        unimplemented!()
    }

    pub fn try_close(&mut self, frame: Frame) -> Result<(), ConnectionError> {
        unimplemented!()
    }
}


/// Handles updates to `SETTINGS_MAX_CONCURRENT_STREAMS`.
///
/// > Indicates the maximum number of concurrent streams that the senderg will allow. This
/// > limit is directional: it applies to the number of streams that the sender permits
/// > the receiver to create. Initially, there is no limit to this value. It is
/// > recommended that this value be no smaller than 100, so as to not unnecessarily limit
/// > parallelism.
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
impl<T, P> ApplySettings for StreamSend<T, P>
    where T: ApplySettings
{
    fn apply_local_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_local_settings(set)
    }

    fn apply_remote_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.max_concurrency = set.max_concurrent_streams();
        self.initial_window_size = set.initial_window_size();
        self.inner.apply_remote_settings(set)
    }
}

impl<T, P> ControlPing for StreamSend<T, P>
    where T: ControlPing
{
    fn start_ping(&mut self, body: PingPayload) -> StartSend<PingPayload, ConnectionError> {
        self.inner.start_ping(body)
    }

    fn take_pong(&mut self) -> Option<PingPayload> {
        self.inner.take_pong()
    }
}

impl<T, P, U> Stream for StreamSend<T, P>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams,
          P: Peer,
{
    type Item = T::Item;
    type Error = T::Error;

    fn poll(&mut self) -> Poll<Option<T::Item>, T::Error> {
        self.inner.poll()
    }
}

impl<T, P, U> Sink for StreamSend<T, P>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams,
          P: Peer,
{
    type SinkItem = T::SinkItem;
    type SinkError = T::SinkError;

    fn start_send(&mut self, item: T::SinkItem) -> StartSend<T::SinkItem, T::SinkError> {
        use frame::Frame::*;

        // Must be enforced through higher levels.
        if let Some(rst) = self.inner.get_reset(item.stream_id()) {
            return Err(User::StreamReset(rst).into());
        }

        match item {
            Headers(v) => {
                let id = v.stream_id();
                let eos = v.is_end_stream();

                // Transition the stream state, creating a new entry if needed
                //
                // TODO: Response can send multiple headers frames before body (1xx
                // responses).
                //
                // ACTUALLY(ver), maybe not?
                //   https://github.com/http2/http2-spec/commit/c83c8d911e6b6226269877e446a5cad8db921784

                // Ensure that sending this frame would not violate the remote's max
                // stream concurrency setting.
                if let Some(max) = self.max_concurrency {
                    let max = max as usize;
                    let streams = self.inner.streams();
                    if !streams.is_active(id) && streams.active_count() >= max - 1 {
                        return Err(User::MaxConcurrencyExceeded.into())
                    }
                }

                let is_closed = {
                    let stream = self.active_streams.entry(id)
                        .or_insert_with(|| StreamState::default());

                    let initialized =
                        stream.send_headers::<P>(eos, self.initial_window_size)?;

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
                    self.active_streams.remove(id);
                    self.reset_streams.insert(id, Reason::NoError);
                }

                self.inner.start_send(Headers(v))
            }

            Data(v) => {
                match self.active_streams.get_mut(v.stream_id()) {
                    None => return Err(User::InactiveStreamId.into()),
                    Some(stream) => {
                        stream.send_data(v.is_end_stream())?;
                        self.inner.start_send(Data(v))
                    }

                }
            }

            Reset(v) => {
                let id = v.stream_id();
                self.active_streams.remove(id);
                self.reset_streams.insert(id, v.reason());
                self.inner.start_send(Reset(v))
            }

            frame => self.inner.start_send(frame),
        }
    }

    fn poll_complete(&mut self) -> Poll<(), T::SinkError> {
        self.inner.poll_complete()
    }
}

impl<T, P, U> ReadySink for StreamSend<T, P>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams,
          T: ReadySink,
          P: Peer,
{
    fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        self.inner.poll_ready()
    }
}
