use super::*;
use {client, frame, proto, server};
use codec::{RecvError, UserError};
use frame::{Reason, DEFAULT_INITIAL_WINDOW_SIZE};
use proto::*;

use http::HeaderMap;

use std::io;
use std::marker::PhantomData;

#[derive(Debug)]
pub(super) struct Recv<B, P>
where
    P: Peer,
{
    /// Initial window size of remote initiated streams
    init_window_sz: WindowSize,

    /// Connection level flow control governing received data
    flow: FlowControl,

    /// The lowest stream ID that is still idle
    next_stream_id: Result<StreamId, StreamIdOverflow>,

    /// The stream ID of the last processed stream
    last_processed_id: StreamId,

    /// Streams that have pending window updates
    pending_window_updates: store::Queue<B, stream::NextWindowUpdate, P>,

    /// New streams to be accepted
    pending_accept: store::Queue<B, stream::NextAccept, P>,

    /// Holds frames that are waiting to be read
    buffer: Buffer<Event<P::Poll>>,

    /// Refused StreamId, this represents a frame that must be sent out.
    refused: Option<StreamId>,

    /// If push promises are allowed to be recevied.
    is_push_enabled: bool,

    _p: PhantomData<B>,
}

#[derive(Debug)]
pub(super) enum Event<T> {
    Headers(T),
    Data(Bytes),
    Trailers(HeaderMap),
}

#[derive(Debug, Clone, Copy)]
struct Indices {
    head: store::Key,
    tail: store::Key,
}

impl<B, P> Recv<B, P>
where
    B: Buf,
    P: Peer,
{
    pub fn new(config: &Config) -> Self {
        let next_stream_id = if P::is_server() { 1 } else { 2 };

        let mut flow = FlowControl::new();

        // connections always have the default window size, regardless of
        // settings
        flow.inc_window(DEFAULT_INITIAL_WINDOW_SIZE)
            .expect("invalid initial remote window size");
        flow.assign_capacity(DEFAULT_INITIAL_WINDOW_SIZE);

        Recv {
            init_window_sz: config.local_init_window_sz,
            flow: flow,
            next_stream_id: Ok(next_stream_id.into()),
            pending_window_updates: store::Queue::new(),
            last_processed_id: StreamId::zero(),
            pending_accept: store::Queue::new(),
            buffer: Buffer::new(),
            refused: None,
            is_push_enabled: config.local_push_enabled,
            _p: PhantomData,
        }
    }

    /// Returns the initial receive window size
    pub fn init_window_sz(&self) -> WindowSize {
        self.init_window_sz
    }

    /// Returns the ID of the last processed stream
    pub fn last_processed_id(&self) -> StreamId {
        self.last_processed_id
    }

    /// Update state reflecting a new, remotely opened stream
    ///
    /// Returns the stream state if successful. `None` if refused
    pub fn open(
        &mut self,
        id: StreamId,
        counts: &mut Counts<P>,
    ) -> Result<Option<StreamId>, RecvError> {
        assert!(self.refused.is_none());

        self.ensure_can_open(id)?;

        let next_id = self.next_stream_id()?;
        if id < next_id {
            return Err(RecvError::Connection(ProtocolError));
        }

        self.next_stream_id = id.next_id();

        if !counts.can_inc_num_recv_streams() {
            self.refused = Some(id);
            return Ok(None);
        }

        Ok(Some(id))
    }

    /// Transition the stream state based on receiving headers
    ///
    /// The caller ensures that the frame represents headers and not trailers.
    pub fn recv_headers(
        &mut self,
        frame: frame::Headers,
        stream: &mut store::Ptr<B, P>,
        counts: &mut Counts<P>,
    ) -> Result<(), RecvError> {
        trace!("opening stream; init_window={}", self.init_window_sz);
        let is_initial = stream.state.recv_open(frame.is_end_stream())?;

        if is_initial {
            let next_id = self.next_stream_id()?;
            if frame.stream_id() >= next_id {
                self.next_stream_id = frame.stream_id().next_id();
            } else {
                return Err(RecvError::Connection(ProtocolError));
            }

            // TODO: be smarter about this logic
            if frame.stream_id() > self.last_processed_id {
                self.last_processed_id = frame.stream_id();
            }

            // Increment the number of concurrent streams
            counts.inc_num_recv_streams();
        }

        if !stream.content_length.is_head() {
            use super::stream::ContentLength;
            use http::header;

            if let Some(content_length) = frame.fields().get(header::CONTENT_LENGTH) {
                let content_length = match parse_u64(content_length.as_bytes()) {
                    Ok(v) => v,
                    Err(_) => {
                        unimplemented!();
                    },
                };

                stream.content_length = ContentLength::Remaining(content_length);
            }
        }

        let message = P::convert_poll_message(frame)?;

        // Push the frame onto the stream's recv buffer
        stream
            .pending_recv
            .push_back(&mut self.buffer, Event::Headers(message));
        stream.notify_recv();

        // Only servers can receive a headers frame that initiates the stream.
        // This is verified in `Streams` before calling this function.
        if P::is_server() {
            self.pending_accept.push(stream);
        }

        Ok(())
    }

    /// Transition the stream based on receiving trailers
    pub fn recv_trailers(
        &mut self,
        frame: frame::Headers,
        stream: &mut store::Ptr<B, P>,
    ) -> Result<(), RecvError> {
        // Transition the state
        stream.state.recv_close()?;

        if stream.ensure_content_length_zero().is_err() {
            return Err(RecvError::Stream {
                id: stream.id,
                reason: ProtocolError,
            });
        }

        let trailers = frame.into_fields();

        // Push the frame onto the stream's recv buffer
        stream
            .pending_recv
            .push_back(&mut self.buffer, Event::Trailers(trailers));
        stream.notify_recv();

        Ok(())
    }

    /// Releases capacity back to the connection
    pub fn release_capacity(
        &mut self,
        capacity: WindowSize,
        stream: &mut store::Ptr<B, P>,
        task: &mut Option<Task>,
    ) -> Result<(), UserError> {
        trace!("release_capacity; size={}", capacity);

        if capacity > stream.in_flight_recv_data {
            return Err(UserError::ReleaseCapacityTooBig);
        }

        // Decrement in-flight data
        stream.in_flight_recv_data -= capacity;

        // Assign capacity to connection & stream
        self.flow.assign_capacity(capacity);
        stream.recv_flow.assign_capacity(capacity);

        if self.flow.unclaimed_capacity().is_some() {
            if let Some(task) = task.take() {
                task.notify();
            }
        }

        if stream.recv_flow.unclaimed_capacity().is_some() {
            // Queue the stream for sending the WINDOW_UPDATE frame.
            self.pending_window_updates.push(stream);

            if let Some(task) = task.take() {
                task.notify();
            }
        }

        Ok(())
    }

    pub fn body_is_empty(&self, stream: &store::Ptr<B, P>) -> bool {
        if !stream.state.is_recv_closed() {
            return false;
        }

        stream
            .pending_recv
            .peek_front(&self.buffer)
            .map(|event| !event.is_data())
            .unwrap_or(true)
    }

    pub fn recv_data(
        &mut self,
        frame: frame::Data,
        stream: &mut store::Ptr<B, P>,
    ) -> Result<(), RecvError> {
        let sz = frame.payload().len();

        if sz > MAX_WINDOW_SIZE as usize {
            unimplemented!();
        }

        let sz = sz as WindowSize;

        if !stream.state.is_recv_streaming() {
            // Receiving a DATA frame when not expecting one is a protocol
            // error.
            return Err(RecvError::Connection(ProtocolError));
        }

        trace!(
            "recv_data; size={}; connection={}; stream={}",
            sz,
            self.flow.window_size(),
            stream.recv_flow.window_size()
        );

        // Ensure that there is enough capacity on the connection before acting
        // on the stream.
        if self.flow.window_size() < sz || stream.recv_flow.window_size() < sz {
            return Err(RecvError::Connection(FlowControlError));
        }

        // Update connection level flow control
        self.flow.send_data(sz);

        // Update stream level flow control
        stream.recv_flow.send_data(sz);

        // Track the data as in-flight
        stream.in_flight_recv_data += sz;

        if stream.dec_content_length(frame.payload().len()).is_err() {
            return Err(RecvError::Stream {
                id: stream.id,
                reason: ProtocolError,
            });
        }

        if frame.is_end_stream() {
            if stream.ensure_content_length_zero().is_err() {
                return Err(RecvError::Stream {
                    id: stream.id,
                    reason: ProtocolError,
                });
            }

            if stream.state.recv_close().is_err() {
                return Err(RecvError::Connection(ProtocolError));
            }
        }

        let event = Event::Data(frame.into_payload());

        // Push the frame onto the recv buffer
        stream.pending_recv.push_back(&mut self.buffer, event);
        stream.notify_recv();

        Ok(())
    }

    pub fn recv_push_promise(
        &mut self,
        frame: frame::PushPromise,
        send: &Send<B, P>,
        stream: store::Key,
        store: &mut Store<B, P>,
    ) -> Result<(), RecvError> {
        // First, make sure that the values are legit
        self.ensure_can_reserve(frame.promised_id())?;

        // Make sure that the stream state is valid
        store[stream]
            .state
            .ensure_recv_open()
            .map_err(|e| e.into_connection_recv_error())?;

        // TODO: Streams in the reserved states do not count towards the concurrency
        // limit. However, it seems like there should be a cap otherwise this
        // could grow in memory indefinitely.

        /*
        if !self.inc_num_streams() {
            self.refused = Some(frame.promised_id());
            return Ok(());
        }
        */

        // TODO: All earlier stream IDs should be implicitly closed.

        // Now, create a new entry for the stream
        let mut new_stream = Stream::new(
            frame.promised_id(),
            send.init_window_sz(),
            self.init_window_sz,
        );

        new_stream.state.reserve_remote()?;

        let mut ppp = store[stream].pending_push_promises.take();

        {
            // Store the stream
            let mut new_stream = store.insert(frame.promised_id(), new_stream);

            ppp.push(&mut new_stream);
        }

        let stream = &mut store[stream];

        stream.pending_push_promises = ppp;
        stream.notify_recv();

        Ok(())
    }

    /// Ensures that `id` is not in the `Idle` state.
    pub fn ensure_not_idle(&self, id: StreamId) -> Result<(), Reason> {
        if let Ok(next) = self.next_stream_id {
            if id >= next {
                return Err(ProtocolError);
            }
        }
        // if next_stream_id is overflowed, that's ok.

        Ok(())
    }

    pub fn recv_reset(
        &mut self,
        frame: frame::Reset,
        stream: &mut Stream<B, P>,
    ) -> Result<(), RecvError> {
        let err = proto::Error::Proto(frame.reason());

        // Notify the stream
        stream.state.recv_err(&err);
        stream.notify_recv();
        Ok(())
    }

    /// Handle a received error
    pub fn recv_err(&mut self, err: &proto::Error, stream: &mut Stream<B, P>) {
        // Receive an error
        stream.state.recv_err(err);

        // If a receiver is waiting, notify it
        stream.notify_recv();
    }

    /// Returns true if the remote peer can initiate a stream with the given ID.
    fn ensure_can_open(&self, id: StreamId) -> Result<(), RecvError> {
        if !P::is_server() {
            // Remote is a server and cannot open streams. PushPromise is
            // registered by reserving, so does not go through this path.
            return Err(RecvError::Connection(ProtocolError));
        }

        // Ensure that the ID is a valid server initiated ID
        if !id.is_client_initiated() {
            return Err(RecvError::Connection(ProtocolError));
        }

        Ok(())
    }

    fn next_stream_id(&self) -> Result<StreamId, RecvError> {
        if let Ok(id) = self.next_stream_id {
            Ok(id)
        } else {
            Err(RecvError::Connection(ProtocolError))
        }
    }

    /// Returns true if the remote peer can reserve a stream with the given ID.
    fn ensure_can_reserve(&self, promised_id: StreamId) -> Result<(), RecvError> {
        // TODO: Are there other rules?
        if P::is_server() {
            // The remote is a client and cannot reserve
            trace!("recv_push_promise; error remote is client");
            return Err(RecvError::Connection(ProtocolError));
        }

        if !promised_id.is_server_initiated() {
            trace!(
                "recv_push_promise; error promised id is invalid {:?}",
                promised_id
            );
            return Err(RecvError::Connection(ProtocolError));
        }

        if !self.is_push_enabled {
            trace!("recv_push_promise; error push is disabled");
            return Err(RecvError::Connection(ProtocolError));
        }

        Ok(())
    }

    /// Send any pending refusals.
    pub fn send_pending_refusal<T>(
        &mut self,
        dst: &mut Codec<T, Prioritized<B>>,
    ) -> Poll<(), io::Error>
    where
        T: AsyncWrite,
    {
        if let Some(stream_id) = self.refused {
            try_ready!(dst.poll_ready());

            // Create the RST_STREAM frame
            let frame = frame::Reset::new(stream_id, RefusedStream);

            // Buffer the frame
            dst.buffer(frame.into())
                .ok()
                .expect("invalid RST_STREAM frame");
        }

        self.refused = None;

        Ok(Async::Ready(()))
    }

    pub fn poll_complete<T>(
        &mut self,
        store: &mut Store<B, P>,
        dst: &mut Codec<T, Prioritized<B>>,
    ) -> Poll<(), io::Error>
    where
        T: AsyncWrite,
    {
        // Send any pending connection level window updates
        try_ready!(self.send_connection_window_update(dst));

        // Send any pending stream level window updates
        try_ready!(self.send_stream_window_updates(store, dst));

        Ok(().into())
    }

    /// Send connection level window update
    fn send_connection_window_update<T>(
        &mut self,
        dst: &mut Codec<T, Prioritized<B>>,
    ) -> Poll<(), io::Error>
    where
        T: AsyncWrite,
    {
        if let Some(incr) = self.flow.unclaimed_capacity() {
            let frame = frame::WindowUpdate::new(StreamId::zero(), incr);

            // Ensure the codec has capacity
            try_ready!(dst.poll_ready());

            // Buffer the WINDOW_UPDATE frame
            dst.buffer(frame.into())
                .ok()
                .expect("invalid WINDOW_UPDATE frame");

            // Update flow control
            self.flow
                .inc_window(incr)
                .ok()
                .expect("unexpected flow control state");
        }

        Ok(().into())
    }


    /// Send stream level window update
    pub fn send_stream_window_updates<T>(
        &mut self,
        store: &mut Store<B, P>,
        dst: &mut Codec<T, Prioritized<B>>,
    ) -> Poll<(), io::Error>
    where
        T: AsyncWrite,
    {
        loop {
            // Ensure the codec has capacity
            try_ready!(dst.poll_ready());

            // Get the next stream
            let mut stream = match self.pending_window_updates.pop(store) {
                Some(stream) => stream,
                None => return Ok(().into()),
            };

            if !stream.state.is_recv_streaming() {
                // No need to send window updates on the stream if the stream is
                // no longer receiving data.
                continue;
            }

            // TODO: de-dup
            if let Some(incr) = stream.recv_flow.unclaimed_capacity() {
                // Create the WINDOW_UPDATE frame
                let frame = frame::WindowUpdate::new(stream.id, incr);

                // Buffer it
                dst.buffer(frame.into())
                    .ok()
                    .expect("invalid WINDOW_UPDATE frame");

                // Update flow control
                stream
                    .recv_flow
                    .inc_window(incr)
                    .ok()
                    .expect("unexpected flow control state");
            }
        }
    }

    pub fn next_incoming(&mut self, store: &mut Store<B, P>) -> Option<store::Key> {
        self.pending_accept.pop(store).map(|ptr| ptr.key())
    }

    pub fn poll_data(&mut self, stream: &mut Stream<B, P>) -> Poll<Option<Bytes>, proto::Error> {
        // TODO: Return error when the stream is reset
        match stream.pending_recv.pop_front(&mut self.buffer) {
            Some(Event::Data(payload)) => Ok(Some(payload).into()),
            Some(event) => {
                // Frame is trailer
                stream.pending_recv.push_front(&mut self.buffer, event);

                // No more data frames
                Ok(None.into())
            },
            None => self.schedule_recv(stream),
        }
    }

    pub fn poll_trailers(
        &mut self,
        stream: &mut Stream<B, P>,
    ) -> Poll<Option<HeaderMap>, proto::Error> {
        match stream.pending_recv.pop_front(&mut self.buffer) {
            Some(Event::Trailers(trailers)) => Ok(Some(trailers).into()),
            Some(_) => {
                // TODO: This is a user error. `poll_trailers` was called before
                // the entire set of data frames have been consumed. What should
                // we do?
                unimplemented!();
            },
            None => self.schedule_recv(stream),
        }
    }

    fn schedule_recv<T>(&mut self, stream: &mut Stream<B, P>) -> Poll<Option<T>, proto::Error> {
        if stream.state.ensure_recv_open()? {
            // Request to get notified once more frames arrive
            stream.recv_task = Some(task::current());
            Ok(Async::NotReady)
        } else {
            // No more frames will be received
            Ok(None.into())
        }
    }
}

impl<B> Recv<B, server::Peer>
where
    B: Buf,
{
    /// TODO: Should this fn return `Result`?
    pub fn take_request(&mut self, stream: &mut store::Ptr<B, server::Peer>) -> Request<()> {
        match stream.pending_recv.pop_front(&mut self.buffer) {
            Some(Event::Headers(request)) => request,
            _ => panic!(),
        }
    }
}

impl<B> Recv<B, client::Peer>
where
    B: Buf,
{
    pub fn poll_response(
        &mut self,
        stream: &mut store::Ptr<B, client::Peer>,
    ) -> Poll<Response<()>, proto::Error> {
        // If the buffer is not empty, then the first frame must be a HEADERS
        // frame or the user violated the contract.
        match stream.pending_recv.pop_front(&mut self.buffer) {
            Some(Event::Headers(response)) => Ok(response.into()),
            Some(_) => unimplemented!(),
            None => {
                stream.state.ensure_recv_open()?;

                stream.recv_task = Some(task::current());
                Ok(Async::NotReady)
            },
        }
    }
}

// ===== impl Event =====

impl<T> Event<T> {
    fn is_data(&self) -> bool {
        match *self {
            Event::Data(..) => true,
            _ => false,
        }
    }
}

// ===== util =====

fn parse_u64(src: &[u8]) -> Result<u64, ()> {
    if src.len() > 19 {
        // At danger for overflow...
        return Err(());
    }

    let mut ret = 0;

    for &d in src {
        if d < b'0' || d > b'9' {
            return Err(());
        }

        ret *= 10;
        ret += (d - b'0') as u64;
    }

    Ok(ret)
}
