use ConnectionError;
use client::Client;
use error::Reason;
use error::User;
use frame::{self, Frame};
use proto::*;
use server::Server;

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
pub struct StreamRecv<T, P> {
    inner: T,
    peer: PhantomData<P>,

    local: StreamMap,
    local_max_concurrency: Option<u32>,
    local_initial_window_size: WindowSize,

    remote: StreamMap,
    remote_max_concurrency: Option<u32>,
    remote_initial_window_size: WindowSize,
    remote_pending_refuse: Option<StreamId>,
}

impl<T, P, U> StreamRecv<T, P>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          P: Peer
{
    pub fn new(initial_window_size: WindowSize,
               max_concurrency: Option<u32>,
               inner: T)
        -> StreamRecv<T, P>
    {
        StreamRecv {
            inner,
            peer: PhantomData,

            local: StreamMap::default(),
            remote: StreamMap::default(),
            max_concurrency,
            initial_window_size,
            remote_pending_refuse: None,
        }
    }

    pub fn try_open_remote(&mut self, frame: Frame) -> Result<(), ConnectionError> {
        unimplemented!()
    }

    pub fn try_close(&mut self, frame: Frame) -> Result<(), ConnectionError> {
        unimplemented!()
    }
}

impl<T, P, U> StreamRecv<T, P>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          P: Peer
{
    fn send_refusal(&mut self, id: StreamId) -> Poll<(), ConnectionError> {
        debug_assert!(self.remote_pending_refused.is_none());

        let f = frame::Reset::new(id, Reason::RefusedStream);
        match self.inner.start_send(f.into())? {
            AsyncSink::Ready => {
                self.reset(id, Reason::RefusedStream);
                Ok(Async::Ready(()))
            }
            AsyncSink::NotReady(_) => {
                self.pending_refused = Some(id);
                Ok(Async::NotReady)
            }
        }
    }
}

impl<T, P> ControlStreams for StreamRecv<T, P>
    where P: Peer
{
   fn local_streams(&self) -> &StreamMap {
        &self.local
    }

    fn local_streams_mut(&mut self) -> &mut StreamMap {
        &mut self.local
    }

    fn remote_streams(&self) -> &StreamMap {
        &self.remote
    }

    fn remote_streams_mut(&mut self) -> &mut StreamMap {
        &mut self.remote
    }

    fn is_valid_local_id(id: StreamId) -> bool {
        P::is_valid_local_stream_id(id)
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
impl<T, P> ApplySettings for StreamRecv<T, P>
    where T: ApplySettings
{
    fn apply_local_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.max_concurrency = set.max_concurrent_streams();
        self.initial_window_size = set.initial_window_size();
        self.inner.apply_local_settings(set)
    }

    fn apply_remote_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_remote_settings(set)
    }
}

impl<T, P> ControlPing for StreamRecv<T, P>
    where T: ControlPing
{
    fn start_ping(&mut self, body: PingPayload) -> StartSend<PingPayload, ConnectionError> {
        self.inner.start_ping(body)
    }

    fn take_pong(&mut self) -> Option<PingPayload> {
        self.inner.take_pong()
    }
}

impl<T, P, U> Stream for StreamRecv<T, P>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          P: Peer,
{
    type Item = T::Item;
    type Error = T::Error;

    fn poll(&mut self) -> Poll<Option<T::Item>, T::Error> {
        use frame::Frame::*;

        // Since there's only one slot for pending refused streams, it must be cleared
        // before polling  a frame from the transport.
        if let Some(id) = self.pending_refused.take() {
            try_ready!(self.send_refusal(id));
        }

        loop {
            match try_ready!(self.inner.poll()) {
                Some(Headers(v)) => {
                    let id = v.stream_id();
                    let eos = v.is_end_stream();

                    if self.get_reset(id).is_some() {
                        // TODO send the remote errors when it sends us frames on reset
                        // streams.
                        continue;
                    }

                    if let Some(mut s) = self.get_active_mut(id) {
                        let created = s.recv_headers(eos, self.initial_window_size)?;
                        assert!(!created);
                        return Ok(Async::Ready(Some(Headers(v))));
                    }

                    // Ensure that receiving this frame will not violate the local max
                    // stream concurrency setting. Ensure that the stream is refused
                    // before processing additional frames.
                    if let Some(max) = self.max_concurrency {
                        let max = max as usize;
                        if !self.local.is_active(id) && self.local.active_count() >= max - 1 {
                            // This frame would violate our local max concurrency, so reject
                            // the stream.
                            try_ready!(self.send_refusal(id));

                            // Try to process another frame (hopefully for an active
                            // stream).
                            continue;
                        }
                    }

                    let is_closed = {
                        let stream = self.active_streams.entry(id)
                            .or_insert_with(|| StreamState::default());

                        let initialized =
                            stream.recv_headers(eos, self.initial_window_size)?;

                        if initialized {
                            if !P::is_valid_remote_stream_id(id) {
                                return Err(Reason::ProtocolError.into());
                            }
                        }

                        stream.is_closed()
                    };

                    if is_closed {
                        self.active_streams.remove(id);
                        self.reset_streams.insert(id, Reason::NoError);
                    }

                    return Ok(Async::Ready(Some(Headers(v))));
                }

                Some(Data(v)) => {
                    let id = v.stream_id();

                    if self.get_reset(id).is_some() {
                        // TODO send the remote errors when it sends us frames on reset
                        // streams.
                        continue;
                    }

                    let is_closed = {
                        let stream = match self.active_streams.get_mut(id) {
                            None => return Err(Reason::ProtocolError.into()),
                            Some(s) => s,
                        };
                        stream.recv_data(v.is_end_stream())?;
                        stream.is_closed()
                    };

                    if is_closed {
                        self.reset(id, Reason::NoError);
                    }

                    return Ok(Async::Ready(Some(Data(v))));
                }

                Some(Reset(v)) => {
                    // Set or update the reset reason.
                    self.reset(v.stream_id(), v.reason());
                    return Ok(Async::Ready(Some(Reset(v))));
                }

                Some(f) => {
                    let id = f.stream_id();

                    if self.get_reset(id).is_some() {
                        // TODO send the remote errors when it sends us frames on reset
                        // streams.
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

impl<T, P, U> Sink for StreamRecv<T, P>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          P: Peer,
{
    type SinkItem = T::SinkItem;
    type SinkError = T::SinkError;

    fn start_send(&mut self, frame: T::SinkItem) -> StartSend<T::SinkItem, T::SinkError> {
        use frame::Frame::*;

        // Must be enforced through higher levels.
        debug_assert!(self.stream_is_reset(item.stream_id()).is_none());

        // The local must complete refusing the remote stream before sending any other
        // frames.
        if let Some(id) = self.pending_refused.take() {
            if self.send_refusal(id)?.is_not_ready() {
                return Ok(AsyncSink::NotReady(item));
            }
        }

        match frame {
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
                if let Some(max) = self.remote_max_concurrency {
                    let max = max as usize;
                    if !self.active_streams.has_stream(id)
                    && self.active_streams.len() >= max - 1 {
                        // This frame would violate our local max concurrency, so reject
                        // the stream.
                        if self.send_refusal(id)?.is_not_ready() {
                            return Ok(AsyncSink::NotReady(Headers(v)));
                        }

                        // Try to process another frame (hopefully for an active
                        // stream).
                        return Err(User::MaxConcurrencyExceeded.into())
                    }
                }

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
        if let Some(id) = self.pending_refused.take() {
            try_ready!(self.send_refusal(id));
        }

        self.inner.poll_complete()
    }
}


impl<T, P, U> ReadySink for StreamRecv<T, P>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ReadySink,
          P: Peer,
{
    fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        if let Some(id) = self.pending_refused.take() {
            try_ready!(self.send_refusal(id));
        }

        self.inner.poll_ready()
    }
}
