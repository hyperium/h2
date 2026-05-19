use super::buffer;
use super::counts;
use super::recv::{self, RecvHeaderBlockError};
use super::send::{self, PreparedReserveCapacity, PreparedSendData, PreparedSendReset};
use super::store::{self, Entry, Resolve, Store};
use super::stream;
use super::{Buffer, Config, Counts, Prioritized, Recv, Send, Stream, StreamId};
use crate::codec::{Codec, SendError, UserError};
use crate::ext::Protocol;
use crate::frame::{self, Frame, Reason};
use crate::proto::{peer, Error, Initiator, Open, Peer, WindowSize};
use crate::{client, proto, server};

use bytes::{Buf, Bytes};
use http::{HeaderMap, Request, Response};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};
use tokio::io::AsyncWrite;

use std::sync::{Arc, Mutex, RwLock};
use std::{fmt, io};

#[derive(Debug)]
pub(crate) struct Streams<B, P>
where
    P: Peer,
{
    /// Holds most of the connection and stream related state for processing
    /// HTTP/2 frames associated with streams.
    inner: Inner,

    /// This is the queue of frames to be written to the wire. This is split out
    /// to avoid requiring a `B` generic on all public API types even if `B` is
    /// not technically required.
    ///
    /// Currently, splitting this out requires a second `Arc` + `Mutex`.
    /// However, it should be possible to avoid this duplication with a little
    /// bit of unsafe code. This optimization has been postponed until it has
    /// been shown to be necessary.
    send_buffer: Arc<SendBuffer<B>>,

    shared: Arc<Shared>,

    _p: ::std::marker::PhantomData<P>,
}

// Like `Streams` but with a `peer::Dyn` field instead of a static `P: Peer` type parameter.
// Ensures that the methods only get one instantiation, instead of two (client and server)
#[derive(Debug)]
pub(crate) struct DynStreams<'a, B> {
    inner: &'a mut Inner,

    send_buffer: &'a SendBuffer<B>,
    shared: &'a Shared,

    peer: peer::Dyn,
}

/// Reference to the stream state
#[derive(Debug)]
pub(crate) struct StreamRef<B> {
    opaque: OpaqueStreamRef,
    send_buffer: Arc<SendBuffer<B>>,
}

/// Reference to the stream state that hides the send data chunk generic
pub(crate) struct OpaqueStreamRef {
    shared: Arc<Shared>,
    stream: Arc<stream::Shared>,
}

/// An injector of streams into the connection.
///
/// Used by the `client::SendRequest`.
pub(crate) struct Injector<B> {
    send_buffer: Arc<SendBuffer<B>>,
    shared: Arc<Shared>,
}

/// Fields needed to manage state related to managing the set of streams. This
/// is mostly split out to make ownership happy.
///
/// TODO: better name
#[derive(Debug)]
struct Inner {
    /// Tracks send & recv stream concurrency.
    counts: Counts,

    /// Connection level state and performs actions on streams
    actions: Actions,

    /// Stores stream state, uniquely owned by the connection task.
    store: Store,
}

#[derive(Debug)]
struct Actions {
    /// Manages state transitions initiated by receiving frames
    recv: Recv,

    /// Manages state transitions initiated by sending frames
    send: Send,
}

/// The new place to store the synchronized state.
///
/// Only things shared are in here, the connection doesn't need to share
/// everything.
#[derive(Debug)]
struct Shared {
    counts: counts::Shared,
    recv: recv::Shared,
    send: send::Shared,

    /// If the connection errors, a copy is kept for any StreamRefs.
    conn_error: RwLock<Option<Error>>,

    queued_request_headers: AtomicUsize,
    pending_ops: Mutex<PendingOps>,

    /// Task that drives connection-level processing.
    conn_task: ConnWaker,
}

/// Contains the buffer of frames to be written to the wire.
#[derive(Debug)]
struct SendBuffer<B> {
    inner: Mutex<Buffer<Frame<B>>>,
}

#[derive(Debug)]
struct PendingOps {
    buffer: Buffer<QueuedOp>,
    queue: buffer::Deque,
}

#[derive(Debug)]
enum ConnOp {
    RequestHeaders {
        frame: frame::Headers,
    },
    ReleaseCapacity {
        capacity: WindowSize,
    },
    PushPromise {
        frame: frame::PushPromise,
        promised: Arc<stream::Shared>,
    },
    Data {
        prepared: PreparedSendData,
        // NOTE: we don't store the frame::Data in here, because of the generic
        // It's stashed in the send buffer, linked in a separate queue on the
        // stream itself.
    },
    DropStreamRef,
    ReserveCapacity {
        prepared: PreparedReserveCapacity,
    },
    Trailers {
        frame: frame::Headers,
    },
    Reset {
        reason: Reason,
        prepared: PreparedSendReset,
    },
    InformationalHeaders {
        frame: frame::Headers,
    },
    ResponseHeaders {
        frame: frame::Headers,
    },
}

#[derive(Debug)]
struct QueuedOp {
    stream: Arc<stream::Shared>,
    op: ConnOp,
}

impl PendingOps {
    fn new() -> Self {
        Self {
            buffer: Buffer::new(),
            queue: buffer::Deque::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    fn push_back(&mut self, op: QueuedOp) {
        self.queue.push_back(&mut self.buffer, op);
    }

    fn pop_front(&mut self) -> Option<QueuedOp> {
        self.queue.pop_front(&mut self.buffer)
    }

    fn for_each_pending_stream<F>(&self, mut f: F)
    where
        F: FnMut(&Arc<stream::Shared>),
    {
        for queued_op in self.queue.iter(&self.buffer) {
            f(&queued_op.stream);
        }
    }

    fn has_queued_request_headers_for(&self, id: StreamId) -> bool {
        self.queue.iter(&self.buffer).any(|queued_op| {
            queued_op.stream.id == id && matches!(queued_op.op, ConnOp::RequestHeaders { .. })
        })
    }

    fn has_queued_frame_for(&self, id: StreamId) -> bool {
        self.queue.iter(&self.buffer).any(|queued_op| {
            queued_op.stream.id == id
                && matches!(
                    queued_op.op,
                    ConnOp::RequestHeaders { .. }
                        | ConnOp::PushPromise { .. }
                        | ConnOp::Data { .. }
                        | ConnOp::Trailers { .. }
                        | ConnOp::Reset { .. }
                        | ConnOp::InformationalHeaders { .. }
                        | ConnOp::ResponseHeaders { .. }
                )
        })
    }
}

/// Assert that there is no pending request for this StreamRef.
#[derive(Debug)]
struct AssertNoPendingRequest;

/// A newtype around the connection waker.
///
/// As a separate type, function arguments can better indicate which waker
/// is expected for any given action.
#[derive(Debug)]
pub(super) struct ConnWaker(atomic_waker::AtomicWaker);

// ===== impl Streams =====

impl<B, P> Streams<B, P>
where
    B: Buf,
    P: Peer,
{
    pub fn new(config: Config) -> Self {
        let peer = P::r#dyn();

        let (recv, recv_shared) = Recv::new(peer, &config);
        let (send, send_shared) = Send::new(&config);
        let counts_shared = counts::Shared::new(&config);

        Streams {
            inner: Inner::new(peer, recv, send, config),
            send_buffer: Arc::new(SendBuffer::new()),
            shared: Arc::new(Shared {
                counts: counts_shared,
                recv: recv_shared,
                send: send_shared,
                conn_error: RwLock::new(None),
                queued_request_headers: AtomicUsize::new(0),
                pending_ops: Mutex::new(PendingOps::new()),
                conn_task: ConnWaker::new(),
            }),
            _p: ::std::marker::PhantomData,
        }
    }

    pub fn set_target_connection_window_size(&mut self, size: WindowSize) -> Result<(), Reason> {
        let me = &mut self.inner;

        me.actions
            .recv
            .set_target_connection_window(size, &self.shared.conn_task)
    }

    pub fn next_incoming(&mut self) -> Option<StreamRef<B>> {
        let me = &mut self.inner;
        let store = &mut me.store;

        me.actions.recv.next_incoming(store).map(|key| {
            let stream = &mut store.resolve(key);
            let shared = stream.shared();
            tracing::trace!(
                "next_incoming; id={:?}, state={:?}",
                stream.id,
                shared.state
            );
            // Pending-accepted remotely-reset streams are counted.
            if shared.state.is_remote_reset() {
                me.counts.dec_num_remote_reset_streams();
            }
            drop(shared);

            StreamRef {
                opaque: OpaqueStreamRef::new(self.shared.clone(), stream),
                send_buffer: self.send_buffer.clone(),
            }
        })
    }

    pub fn send_pending_refusal<T>(
        &mut self,
        cx: &mut Context,
        dst: &mut Codec<T, Prioritized<B>>,
    ) -> Poll<io::Result<()>>
    where
        T: AsyncWrite + Unpin,
    {
        self.inner.actions.recv.send_pending_refusal(cx, dst)
    }

    pub fn clear_expired_reset_streams(&mut self) {
        let me = &mut self.inner;
        me.actions.recv.clear_expired_reset_streams(
            &self.shared.counts,
            &mut me.store,
            &mut me.counts,
        );
    }

    pub fn poll_complete<T>(
        &mut self,
        cx: &mut Context,
        dst: &mut Codec<T, Prioritized<B>>,
    ) -> Poll<io::Result<()>>
    where
        T: AsyncWrite + Unpin,
    {
        self.inner
            .poll_complete(&self.send_buffer, &self.shared, cx, dst)
    }

    pub fn apply_remote_settings(
        &mut self,
        frame: &frame::Settings,
        is_initial: bool,
    ) -> Result<(), Error> {
        let me = &mut self.inner;

        let mut send_buffer = self.send_buffer.inner.lock().unwrap();
        let send_buffer = &mut *send_buffer;

        me.counts
            .apply_remote_settings(&self.shared.counts, frame, is_initial);

        me.actions.send.apply_remote_settings(
            &self.shared.send,
            frame,
            send_buffer,
            &mut me.store,
            &self.shared.counts,
            &mut me.counts,
            &self.shared.conn_task,
        )
    }

    pub fn apply_local_settings(&mut self, frame: &frame::Settings) -> Result<(), Error> {
        let me = &mut self.inner;
        me.actions
            .recv
            .apply_local_settings(&self.shared.recv, frame, &mut me.store)
    }

    pub fn injector(&self) -> Injector<B> {
        Injector {
            shared: self.shared.clone(),
            send_buffer: self.send_buffer.clone(),
        }
    }
}

impl<B> DynStreams<'_, B> {
    pub fn is_buffer_empty(&self) -> bool {
        self.send_buffer.is_empty() && self.shared.pending_ops.lock().unwrap().is_empty()
    }

    pub fn is_server(&self) -> bool {
        self.peer.is_server()
    }

    pub fn recv_headers(&mut self, frame: frame::Headers) -> Result<(), Error> {
        self.inner
            .recv_headers(self.peer, self.send_buffer, self.shared, frame)
    }

    pub fn recv_data(&mut self, frame: frame::Data) -> Result<(), Error> {
        self.inner
            .recv_data(self.peer, self.send_buffer, self.shared, frame)
    }

    pub fn recv_reset(&mut self, frame: frame::Reset) -> Result<(), Error> {
        self.inner.recv_reset(self.send_buffer, self.shared, frame)
    }

    /// Notify all streams that a connection-level error happened.
    pub fn handle_error(&mut self, err: proto::Error) -> StreamId {
        self.inner.handle_error(self.send_buffer, self.shared, err)
    }

    pub fn recv_go_away(&mut self, frame: &frame::GoAway) -> Result<(), Error> {
        self.inner
            .recv_go_away(self.send_buffer, self.shared, frame)
    }

    pub fn last_processed_id(&self) -> StreamId {
        self.inner.actions.recv.last_processed_id()
    }

    pub fn recv_window_update(&mut self, frame: frame::WindowUpdate) -> Result<(), Error> {
        self.inner
            .recv_window_update(self.send_buffer, self.shared, frame)
    }

    pub fn recv_push_promise(&mut self, frame: frame::PushPromise) -> Result<(), Error> {
        self.inner
            .recv_push_promise(self.send_buffer, self.shared, frame)
    }

    pub fn recv_eof(&mut self, clear_pending_accept: bool) -> Result<(), ()> {
        self.inner
            .recv_eof(self.send_buffer, self.shared, clear_pending_accept)
    }

    pub fn send_reset(
        &mut self,
        id: StreamId,
        reason: Reason,
    ) -> Result<(), crate::proto::error::GoAway> {
        self.inner
            .send_reset(self.send_buffer, self.shared, id, reason)
    }

    pub fn send_go_away(&mut self, last_processed_id: StreamId) {
        self.inner.actions.recv.go_away(last_processed_id);
    }
}

fn reconcile_deferred_recv_window(stream: &mut Stream, target: WindowSize) {
    let mut recv = stream.recv();
    let current = recv.recv_flow.window_size();

    match target.cmp(&current) {
        std::cmp::Ordering::Less => {
            let dec = current - target;
            let res = recv.recv_flow.dec_recv_window(dec);
            debug_assert!(
                res.is_ok(),
                "deferred recv window decrement should be valid"
            );
        }
        std::cmp::Ordering::Greater => {
            let inc = target - current;
            let res = recv.recv_flow.inc_window(inc);
            debug_assert!(
                res.is_ok(),
                "deferred recv window increment should be valid"
            );
            let res = recv.recv_flow.assign_capacity(inc);
            debug_assert!(
                res.is_ok(),
                "deferred recv capacity assignment should be valid"
            );
        }
        std::cmp::Ordering::Equal => {}
    }
}

fn reconcile_deferred_send_window(stream: &mut Stream, target: WindowSize) {
    let mut send = stream.send();
    let current = send.send_flow.window_size();

    match target.cmp(&current) {
        std::cmp::Ordering::Less => {
            let dec = current - target;
            let res = send.send_flow.dec_send_window(dec);
            debug_assert!(
                res.is_ok(),
                "deferred send window decrement should be valid"
            );
        }
        std::cmp::Ordering::Greater => {
            let inc = target - current;
            let res = send.send_flow.inc_window(inc);
            debug_assert!(
                res.is_ok(),
                "deferred send window increment should be valid"
            );
        }
        std::cmp::Ordering::Equal => {}
    }
}

impl Inner {
    fn new(peer: peer::Dyn, recv: Recv, send: Send, config: Config) -> Self {
        Inner {
            counts: Counts::new(peer, &config),
            actions: Actions { recv, send },
            store: Store::new(),
        }
    }

    fn recv_headers<B>(
        &mut self,
        peer: peer::Dyn,
        send_buffer: &SendBuffer<B>,
        shared: &Shared,
        frame: frame::Headers,
    ) -> Result<(), Error> {
        let id = frame.stream_id();
        let store = &mut self.store;

        // The GOAWAY process has begun. All streams with a greater ID than
        // specified as part of GOAWAY should be ignored.
        if id > self.actions.recv.max_stream_id() {
            tracing::trace!(
                "id ({:?}) > max_stream_id ({:?}), ignoring HEADERS",
                id,
                self.actions.recv.max_stream_id()
            );
            return Ok(());
        }

        let key = match store.find_entry(id) {
            Entry::Occupied(e) => e.key(),
            Entry::Vacant(e) => {
                // Client: it's possible to send a request, and then send
                // a RST_STREAM while the response HEADERS were in transit.
                //
                // Server: we can't reset a stream before having received
                // the request headers, so don't allow.
                if !peer.is_server() {
                    // This may be response headers for a stream we've already
                    // forgotten about...
                    if self.actions.may_have_forgotten_stream(peer, shared, id) {
                        tracing::debug!(
                            "recv_headers for old stream={:?}, sending STREAM_CLOSED",
                            id,
                        );
                        return Err(Error::library_reset(id, Reason::STREAM_CLOSED));
                    }
                }

                match self
                    .actions
                    .recv
                    .open(id, Open::Headers, &shared.counts, &mut self.counts)?
                {
                    Some(stream_id) => {
                        let stream = Stream::new(
                            stream_id,
                            shared.send.init_window_sz(),
                            shared.recv.init_window_sz(),
                        );

                        let key = e.insert(stream);
                        shared.counts.inc_num_wired_streams();
                        key
                    }
                    None => return Ok(()),
                }
            }
        };

        let stream = store.resolve(key);

        if stream.send().is_pending_open {
            proto_err!(conn: "recv_headers: received frame on idle stream {:?}", id);
            return Err(Error::library_go_away(Reason::PROTOCOL_ERROR));
        }

        if stream.shared().state.is_local_error() {
            // Locally reset streams must ignore frames "for some time".
            // This is because the remote may have sent trailers before
            // receiving the RST_STREAM frame.
            tracing::trace!("recv_headers; ignoring trailers on {:?}", stream.id);
            return Ok(());
        }

        let actions = &mut self.actions;
        let mut send_buffer = send_buffer.inner.lock().unwrap();
        let send_buffer = &mut *send_buffer;

        self.counts.transition(&shared.counts, stream, |counts, stream| {
            tracing::trace!(
                "recv_headers; stream={:?}; state={:?}",
                stream.id,
                stream.shared().state
            );

            let res = if stream.shared().state.is_recv_headers() {
                match actions
                    .recv
                    .recv_headers(&shared.recv, frame, stream, &shared.counts, counts)
                {
                    Ok(()) => Ok(()),
                    Err(RecvHeaderBlockError::Oversize(resp)) => {
                        if let Some(resp) = resp {
                            let sent = actions.send.send_headers(
                                &shared.send,
                                resp,
                                send_buffer,
                                stream,
                                counts,
                                &shared.conn_task,
                            );
                            debug_assert!(sent.is_ok(), "oversize response should not fail");

                            actions.send.schedule_implicit_reset(
                                stream,
                                Reason::PROTOCOL_ERROR,
                                &shared.counts,
                                counts,
                                &shared.conn_task,
                            );

                            actions.recv.enqueue_reset_expiration(stream, counts);

                            Ok(())
                        } else {
                            Err(Error::library_reset(stream.id, Reason::PROTOCOL_ERROR))
                        }
                    },
                    Err(RecvHeaderBlockError::State(err)) => Err(err),
                }
            } else {
                if !frame.is_end_stream() {
                    // Receiving trailers that don't set EOS is a "malformed"
                    // message. Malformed messages are a stream error.
                    proto_err!(stream: "recv_headers: trailers frame was not EOS; stream={:?}", stream.id);
                    return Err(Error::library_reset(stream.id, Reason::PROTOCOL_ERROR));
                }

                actions.recv.recv_trailers(&shared.recv, frame, stream)
            };

            actions.reset_on_recv_stream_err(
                send_buffer,
                stream,
                &shared.counts,
                counts,
                res,
                &shared.conn_task,
            )
        })
    }

    fn recv_data<B>(
        &mut self,
        peer: peer::Dyn,
        send_buffer: &SendBuffer<B>,
        shared: &Shared,
        frame: frame::Data,
    ) -> Result<(), Error> {
        let id = frame.stream_id();
        let store = &mut self.store;

        let stream = match store.find_mut(&id) {
            Some(stream) => stream,
            None => {
                // The GOAWAY process has begun. All streams with a greater ID
                // than specified as part of GOAWAY should be ignored.
                if id > self.actions.recv.max_stream_id() {
                    tracing::trace!(
                        "id ({:?}) > max_stream_id ({:?}), ignoring DATA",
                        id,
                        self.actions.recv.max_stream_id()
                    );

                    // We still need to account for connection-level flow control.
                    let sz = frame.flow_controlled_len();
                    assert!(sz <= super::MAX_WINDOW_SIZE as usize);
                    let sz = sz as WindowSize;
                    self.actions.recv.ignore_data(sz, &shared.conn_task)?;

                    return Ok(());
                }

                if self.actions.may_have_forgotten_stream(peer, shared, id) {
                    tracing::debug!("recv_data for old stream={:?}, sending STREAM_CLOSED", id,);

                    let sz = frame.flow_controlled_len();
                    // This should have been enforced at the codec::FramedRead layer, so
                    // this is just a sanity check.
                    assert!(sz <= super::MAX_WINDOW_SIZE as usize);
                    let sz = sz as WindowSize;

                    self.actions.recv.ignore_data(sz, &shared.conn_task)?;
                    return Err(Error::library_reset(id, Reason::STREAM_CLOSED));
                }

                proto_err!(conn: "recv_data: stream not found; id={:?}", id);
                return Err(Error::library_go_away(Reason::PROTOCOL_ERROR));
            }
        };

        let actions = &mut self.actions;
        let mut send_buffer = send_buffer.inner.lock().unwrap();
        let send_buffer = &mut *send_buffer;

        self.counts
            .transition(&shared.counts, stream, |counts, stream| {
                let sz = frame.flow_controlled_len();
                let res = actions
                    .recv
                    .recv_data(&shared.recv, &shared.conn_task, frame, stream);

                // Any stream error after receiving a DATA frame means
                // we won't give the data to the user, and so they can't
                // release the capacity. We do it automatically.
                if let Err(Error::Reset(..)) = res {
                    actions
                        .recv
                        .release_connection_capacity(sz as WindowSize, &shared.conn_task);
                }
                actions.reset_on_recv_stream_err(
                    send_buffer,
                    stream,
                    &shared.counts,
                    counts,
                    res,
                    &shared.conn_task,
                )
            })
    }

    fn recv_reset<B>(
        &mut self,
        send_buffer: &SendBuffer<B>,
        shared: &Shared,
        frame: frame::Reset,
    ) -> Result<(), Error> {
        let id = frame.stream_id();
        let store = &mut self.store;

        if id.is_zero() {
            proto_err!(conn: "recv_reset: invalid stream ID 0");
            return Err(Error::library_go_away(Reason::PROTOCOL_ERROR));
        }

        // The GOAWAY process has begun. All streams with a greater ID than
        // specified as part of GOAWAY should be ignored.
        if id > self.actions.recv.max_stream_id() {
            tracing::trace!(
                "id ({:?}) > max_stream_id ({:?}), ignoring RST_STREAM",
                id,
                self.actions.recv.max_stream_id()
            );
            return Ok(());
        }

        let stream = match store.find_mut(&id) {
            Some(stream) => stream,
            None => {
                // TODO: Are there other error cases?
                self.actions
                    .ensure_not_idle(self.counts.peer(), shared, id)
                    .map_err(Error::library_go_away)?;

                return Ok(());
            }
        };

        if stream.send().is_pending_open {
            proto_err!(conn: "recv_reset: received frame on idle stream {:?}", id);
            return Err(Error::library_go_away(Reason::PROTOCOL_ERROR));
        }

        let mut send_buffer = send_buffer.inner.lock().unwrap();
        let send_buffer = &mut *send_buffer;

        let actions = &mut self.actions;

        self.counts
            .transition(&shared.counts, stream, |counts, stream| {
                let has_queued_frame =
                    stream.inner().is_pending_send || shared.has_queued_frame_for(stream.id);
                actions
                    .recv
                    .recv_reset(frame, stream, counts, has_queued_frame)?;
                actions
                    .send
                    .handle_error(send_buffer, stream, &shared.counts, counts);
                assert!(stream.shared().state.is_closed());
                Ok(())
            })
    }

    fn recv_window_update<B>(
        &mut self,
        send_buffer: &SendBuffer<B>,
        shared: &Shared,
        frame: frame::WindowUpdate,
    ) -> Result<(), Error> {
        let id = frame.stream_id();
        let store = &mut self.store;

        let mut send_buffer = send_buffer.inner.lock().unwrap();
        let send_buffer = &mut *send_buffer;

        if id.is_zero() {
            self.actions
                .send
                .recv_connection_window_update(frame, &mut *store, &shared.counts, &mut self.counts)
                .map_err(Error::library_go_away)?;
        } else {
            // The remote may send window updates for streams that the local now
            // considers closed. It's ok...
            if let Some(mut stream) = store.find_mut(&id) {
                if stream.send().is_pending_open {
                    proto_err!(conn: "recv_window_update: received frame on idle stream {:?}", id);
                    return Err(Error::library_go_away(Reason::PROTOCOL_ERROR));
                }

                let res = self
                    .actions
                    .send
                    .recv_stream_window_update(
                        frame.size_increment(),
                        send_buffer,
                        &mut stream,
                        &shared.counts,
                        &mut self.counts,
                        &shared.conn_task,
                    )
                    .map_err(|reason| Error::library_reset(id, reason));

                return self.actions.reset_on_recv_stream_err(
                    send_buffer,
                    &mut stream,
                    &shared.counts,
                    &mut self.counts,
                    res,
                    &shared.conn_task,
                );
            } else {
                self.actions
                    .ensure_not_idle(self.counts.peer(), shared, id)
                    .map_err(Error::library_go_away)?;
            }
        }

        Ok(())
    }

    fn handle_error<B>(
        &mut self,
        send_buffer: &SendBuffer<B>,
        shared: &Shared,
        err: proto::Error,
    ) -> StreamId {
        let actions = &mut self.actions;
        let counts = &mut self.counts;
        let mut send_buffer = send_buffer.inner.lock().unwrap();
        let send_buffer = &mut *send_buffer;
        let store = &mut self.store;

        let last_processed_id = actions.recv.last_processed_id();

        store.for_each(|stream| {
            counts.transition(&shared.counts, stream, |counts, stream| {
                actions.recv.handle_error(&err, &*stream);
                actions
                    .send
                    .handle_error(send_buffer, stream, &shared.counts, counts);
            })
        });

        *shared.conn_error.write().unwrap() = Some(err);
        if let Some(err) = shared.conn_error.read().unwrap().as_ref() {
            shared
                .pending_ops
                .lock()
                .unwrap()
                .for_each_pending_stream(|stream| stream.handle_error(err));
        }

        last_processed_id
    }

    fn recv_go_away<B>(
        &mut self,
        send_buffer: &SendBuffer<B>,
        shared: &Shared,
        frame: &frame::GoAway,
    ) -> Result<(), Error> {
        let actions = &mut self.actions;
        let counts = &mut self.counts;
        let mut send_buffer = send_buffer.inner.lock().unwrap();
        let send_buffer = &mut *send_buffer;
        let store = &mut self.store;

        let last_stream_id = frame.last_stream_id();

        actions.send.recv_go_away(last_stream_id)?;

        let err = Error::remote_go_away(frame.debug_data().clone(), frame.reason());

        let peer = counts.peer();
        store.for_each(|stream| {
            if stream.id > last_stream_id && peer.is_local_init(stream.id) {
                counts.transition(&shared.counts, stream, |counts, stream| {
                    actions.recv.handle_error(&err, &*stream);
                    actions
                        .send
                        .handle_error(send_buffer, stream, &shared.counts, counts);
                })
            }
        });

        *shared.conn_error.write().unwrap() = Some(err);
        if let Some(err) = shared.conn_error.read().unwrap().as_ref() {
            shared
                .pending_ops
                .lock()
                .unwrap()
                .for_each_pending_stream(|stream| {
                    if peer.is_local_init(stream.id) && stream.id > last_stream_id {
                        stream.handle_error(err);
                    }
                });
        }

        Ok(())
    }

    fn recv_push_promise<B>(
        &mut self,
        send_buffer: &SendBuffer<B>,
        shared: &Shared,
        frame: frame::PushPromise,
    ) -> Result<(), Error> {
        let id = frame.stream_id();
        let promised_id = frame.promised_id();
        let store = &mut self.store;

        // First, ensure that the initiating stream is still in a valid state.
        let parent_key = match store.find_mut(&id) {
            Some(stream) => {
                // The GOAWAY process has begun. All streams with a greater ID
                // than specified as part of GOAWAY should be ignored.
                if id > self.actions.recv.max_stream_id() {
                    tracing::trace!(
                        "id ({:?}) > max_stream_id ({:?}), ignoring PUSH_PROMISE",
                        id,
                        self.actions.recv.max_stream_id()
                    );
                    return Ok(());
                }

                // The stream must be receive open
                if !stream.shared().state.ensure_recv_open()? {
                    proto_err!(conn: "recv_push_promise: initiating stream is not opened");
                    return Err(Error::library_go_away(Reason::PROTOCOL_ERROR));
                }

                stream.key()
            }
            None => {
                proto_err!(conn: "recv_push_promise: initiating stream is in an invalid state");
                return Err(Error::library_go_away(Reason::PROTOCOL_ERROR));
            }
        };

        // TODO: Streams in the reserved states do not count towards the concurrency
        // limit. However, it seems like there should be a cap otherwise this
        // could grow in memory indefinitely.

        // Ensure that we can reserve streams
        self.actions.recv.ensure_can_reserve(&shared.recv)?;

        // Next, open the stream.
        //
        // If `None` is returned, then the stream is being refused. There is no
        // further work to be done.
        if self
            .actions
            .recv
            .open(
                promised_id,
                Open::PushPromise,
                &shared.counts,
                &mut self.counts,
            )?
            .is_none()
        {
            return Ok(());
        }

        // Try to handle the frame and create a corresponding key for the pushed stream
        // this requires a bit of indirection to make the borrow checker happy.
        let child_stream: Option<Arc<stream::Shared>> = {
            // Create state for the stream
            let stream = store.insert(promised_id, {
                Stream::new(
                    promised_id,
                    shared.send.init_window_sz(),
                    shared.recv.init_window_sz(),
                )
            });
            shared.counts.inc_num_wired_streams();

            let actions = &mut self.actions;

            self.counts
                .transition(&shared.counts, stream, |counts, stream| {
                    let stream_valid = actions.recv.recv_push_promise(&shared.recv, frame, stream);

                    match stream_valid {
                        Ok(()) => Ok(Some(stream.clone_shared())),
                        _ => {
                            let mut send_buffer = send_buffer.inner.lock().unwrap();
                            actions
                                .reset_on_recv_stream_err(
                                    &mut *send_buffer,
                                    stream,
                                    &shared.counts,
                                    counts,
                                    stream_valid,
                                    &shared.conn_task,
                                )
                                .map(|()| None)
                        }
                    }
                })?
        };
        // If we're successful, push the headers and stream...
        if let Some(child) = child_stream {
            let parent = &mut store.resolve(parent_key);
            shared.recv.push_pushed(parent.shared_ref(), child);
            parent.notify_push();
        };

        Ok(())
    }

    fn recv_eof<B>(
        &mut self,
        send_buffer: &SendBuffer<B>,
        shared: &Shared,
        clear_pending_accept: bool,
    ) -> Result<(), ()> {
        let actions = &mut self.actions;
        let counts = &mut self.counts;
        let mut send_buffer = send_buffer.inner.lock().unwrap();
        let send_buffer = &mut *send_buffer;
        let store = &mut self.store;

        if shared.conn_error.read().unwrap().is_none() {
            *shared.conn_error.write().unwrap() = Some(
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "connection closed because of a broken pipe",
                )
                .into(),
            );
        }

        tracing::trace!("Streams::recv_eof");

        store.for_each(|stream| {
            counts.transition(&shared.counts, stream, |counts, stream| {
                actions.recv.recv_eof(stream);

                // This handles resetting send state associated with the
                // stream
                actions
                    .send
                    .handle_error(send_buffer, stream, &shared.counts, counts);
            })
        });

        if let Some(err) = shared.conn_error.read().unwrap().as_ref() {
            shared
                .pending_ops
                .lock()
                .unwrap()
                .for_each_pending_stream(|stream| stream.handle_error(err));
        }

        actions.clear_queues(clear_pending_accept, &shared.counts, store, counts);
        Ok(())
    }

    fn poll_complete<T, B>(
        &mut self,
        send_buffer: &SendBuffer<B>,
        shared: &Shared,
        cx: &mut Context,
        dst: &mut Codec<T, Prioritized<B>>,
    ) -> Poll<io::Result<()>>
    where
        T: AsyncWrite + Unpin,
        B: Buf,
    {
        self.process_pending_conn_ops(send_buffer, shared);

        // Send WINDOW_UPDATE frames first
        //
        // TODO: It would probably be better to interleave updates w/ data
        // frames.
        {
            ready!(self.actions.recv.poll_complete(
                cx,
                &mut self.store,
                &shared.counts,
                &mut self.counts,
                dst
            ))?;
        }

        // Send any other pending frames
        let mut send_buffer = send_buffer.inner.lock().unwrap();
        let send_buffer = &mut *send_buffer;
        {
            ready!(self.actions.send.poll_complete(
                cx,
                send_buffer,
                &mut self.store,
                &shared.counts,
                &mut self.counts,
                dst
            ))?;
        }

        // Nothing else to do, track the task
        shared.conn_task.register(cx.waker());

        Poll::Ready(Ok(()))
    }

    fn process_pending_conn_ops<B>(&mut self, send_buffer: &SendBuffer<B>, shared: &Shared)
    where
        B: Buf,
    {
        let actions = &mut self.actions;
        let counts = &mut self.counts;

        while let Some(queued_op) = {
            let mut pending_ops = shared.pending_ops.lock().unwrap();
            pending_ops.pop_front()
        } {
            let QueuedOp { stream, op } = queued_op;
            if matches!(&op, ConnOp::RequestHeaders { .. }) {
                let prev = shared.queued_request_headers.fetch_sub(1, Ordering::Relaxed);
                debug_assert!(prev > 0, "queued_request_headers must stay in sync");
            }
            match op {
                ConnOp::DropStreamRef => {
                    stream.ops_dec();
                    release_queued_send_ref(
                        actions,
                        counts,
                        shared,
                        &shared.counts,
                        &mut self.store,
                        &stream,
                    );
                }
                ConnOp::Reset { reason, prepared } => {
                    stream.ops_dec();
                    let Some(mut stream_ptr) = self.store.find_mut_even_if_unlinked(&stream)
                    else {
                        continue;
                    };
                    let mut send_frames_guard = send_buffer.inner.lock().unwrap();
                    let send_frames = &mut *send_frames_guard;

                    actions.send.send_prepared_reset(
                        reason,
                        prepared,
                        send_frames,
                        &mut stream_ptr,
                        &shared.counts,
                        counts,
                        &shared.conn_task,
                    );
                    drop(stream_ptr);
                    if prepared.send_explicit_reset {
                    } else {
                        release_queued_send_ref(
                            actions,
                            counts,
                            shared,
                            &shared.counts,
                            &mut self.store,
                            &stream,
                        );
                    }
                }
                other => {
                    stream.ops_dec();
                    match &other {
                        ConnOp::RequestHeaders { frame } => {
                            if self.store.find_mut_even_if_unlinked(&stream).is_none() {
                                let (
                                    is_unreferenced,
                                    is_closed_without_reset,
                                    is_ignored_by_go_away,
                                ) = {
                                    let shared_state = stream.state();
                                    (
                                        shared_state.ref_count == 0,
                                        shared_state.state.is_closed()
                                            && !shared_state.state.is_local_reset(),
                                        counts.peer().is_local_init(stream.id)
                                            && actions.send.is_ignored_by_go_away(stream.id),
                                    )
                                };

                                if is_ignored_by_go_away {
                                    if let Some(err) = shared.conn_error.read().unwrap().as_ref() {
                                        stream.handle_error(err);
                                    }
                                }

                                if is_unreferenced
                                    || is_closed_without_reset
                                    || is_ignored_by_go_away
                                {
                                    continue;
                                }

                                let mut stream = Stream::new_with_shared(stream.clone());
                                reconcile_deferred_send_window(
                                    &mut stream,
                                    shared.send.init_window_sz(),
                                );
                                reconcile_deferred_recv_window(
                                    &mut stream,
                                    shared.recv.init_window_sz(),
                                );
                                if frame.pseudo().method == Some(http::Method::HEAD) {
                                    stream.inner_mut().content_length = stream::ContentLength::Head;
                                }
                                self.store.insert(stream.id, stream);
                                shared.counts.inc_num_wired_streams();
                            }
                        }
                        ConnOp::PushPromise { promised, .. } => {
                            if self.store.find_mut_even_if_unlinked(&promised).is_none() {
                                let mut stream = Stream::new_with_shared(promised.clone());
                                reconcile_deferred_send_window(
                                    &mut stream,
                                    shared.send.init_window_sz(),
                                );
                                reconcile_deferred_recv_window(
                                    &mut stream,
                                    shared.recv.init_window_sz(),
                                );
                                self.store.insert(stream.id, stream);
                                shared.counts.inc_num_wired_streams();
                            }
                        }
                        _ => {}
                    }
                    let Some(mut stream_ptr) = self.store.find_mut_even_if_unlinked(&stream)
                    else {
                        continue;
                    };

                    let drop_if_stream_reset = matches!(
                        &other,
                        ConnOp::Data { .. }
                            | ConnOp::Trailers { .. }
                            | ConnOp::InformationalHeaders { .. }
                            | ConnOp::ResponseHeaders { .. }
                    );
                    let stream_is_reset = {
                        let shared_state = stream_ptr.shared();
                        shared_state.state.is_reset()
                    };
                    if drop_if_stream_reset && stream_is_reset {
                        if let ConnOp::Data { .. } = other {
                            let mut send_frames_guard = send_buffer.inner.lock().unwrap();
                            let send_frames = &mut *send_frames_guard;
                            let frame = stream_ptr.send().pending_op_data.pop_front(send_frames);
                            debug_assert!(
                                matches!(frame, None | Some(Frame::Data(_))),
                                "pending_op_data must only contain DATA frames"
                            );
                        }

                        let is_pending_reset = stream_ptr.is_pending_reset_expiration();
                        counts.transition_after(&shared.counts, stream_ptr, is_pending_reset);
                        continue;
                    }

                    match other {
                        ConnOp::RequestHeaders { frame } => {
                            let is_pending_reset = stream_ptr.is_pending_reset_expiration();
                            let should_send = {
                                stream_ptr.notify_send();
                                let shared = stream_ptr.shared();
                                !shared.state.is_closed() || shared.state.is_local_reset()
                            };
                            if !should_send {
                                counts.transition_after(
                                    &shared.counts,
                                    stream_ptr,
                                    is_pending_reset,
                                );
                                continue;
                            }
                            let mut send_frames_guard = send_buffer.inner.lock().unwrap();
                            let send_frames = &mut *send_frames_guard;
                            actions.send.send_headers_after_prepare(
                                &shared.send,
                                frame,
                                send_frames,
                                &mut stream_ptr,
                                counts,
                                &shared.conn_task,
                            );
                        }
                        ConnOp::ReleaseCapacity { capacity } => {
                            actions
                                .recv
                                .release_connection_capacity(capacity, &shared.conn_task);
                            actions
                                .recv
                                .schedule_stream_window_update(&mut stream_ptr, &shared.conn_task);
                        }
                        ConnOp::Data { prepared } => {
                            let mut send_frames_guard = send_buffer.inner.lock().unwrap();
                            let send_frames = &mut *send_frames_guard;
                            let frame = stream_ptr.send().pending_op_data.pop_front(send_frames);
                            match frame {
                                Some(Frame::Data(frame)) => {
                                    actions.send.send_prepared_data(
                                        frame,
                                        prepared,
                                        send_frames,
                                        &mut stream_ptr,
                                        &shared.counts,
                                        counts,
                                        &shared.conn_task,
                                    );
                                }
                                Some(frame) => {
                                    unreachable!(
                                        "pending_op_data must only contain DATA frames: {frame:?}"
                                    )
                                }
                                None => {}
                            }
                        }
                        ConnOp::ReserveCapacity { prepared } => {
                            actions.send.apply_reserve_capacity(
                                prepared,
                                &mut stream_ptr,
                                &shared.counts,
                                counts,
                            );
                        }
                        ConnOp::PushPromise { frame, promised: _ } => {
                            let mut send_frames_guard = send_buffer.inner.lock().unwrap();
                            let send_frames = &mut *send_frames_guard;
                            actions.send.send_push_promise_frame(
                                &shared.send,
                                frame,
                                send_frames,
                                &mut stream_ptr,
                                &shared.conn_task,
                            );
                        }
                        ConnOp::Trailers { frame } => {
                            let mut send_frames_guard = send_buffer.inner.lock().unwrap();
                            let send_frames = &mut *send_frames_guard;
                            actions.send.send_trailers_frame(
                                frame,
                                send_frames,
                                &mut stream_ptr,
                                &shared.counts,
                                counts,
                                &shared.conn_task,
                            );
                        }
                        ConnOp::InformationalHeaders { frame } => {
                            let mut send_frames_guard = send_buffer.inner.lock().unwrap();
                            let send_frames = &mut *send_frames_guard;
                            actions.send.send_interim_informational_headers_frame(
                                frame,
                                send_frames,
                                &mut stream_ptr,
                                &shared.conn_task,
                            );
                        }
                        ConnOp::ResponseHeaders { frame } => {
                            let mut send_frames_guard = send_buffer.inner.lock().unwrap();
                            let send_frames = &mut *send_frames_guard;
                            actions.send.send_headers_frame(
                                &shared.send,
                                frame,
                                send_frames,
                                &mut stream_ptr,
                                &shared.conn_task,
                            );
                        }
                        ConnOp::DropStreamRef | ConnOp::Reset { .. } => unreachable!(),
                    }
                }
            }
        }
    }

    fn send_reset<B>(
        &mut self,
        send_buffer: &SendBuffer<B>,
        shared: &Shared,
        id: StreamId,
        reason: Reason,
    ) -> Result<(), crate::proto::error::GoAway> {
        let key = match self.store.find_entry(id) {
            Entry::Occupied(e) => e.key(),
            Entry::Vacant(e) => {
                // Resetting a stream we don't know about? That could be OK...
                //
                // 1. As a server, we just received a request, but that request
                //    was bad, so we're resetting before even accepting it.
                //    This is totally fine.
                //
                // 2. The remote may have sent us a frame on new stream that
                //    it's *not* supposed to have done, and thus, we don't know
                //    the stream. In that case, sending a reset will "open" the
                //    stream in our store. Maybe that should be a connection
                //    error instead? At least for now, we need to update what
                //    our vision of the next stream is.
                if self.counts.peer().is_local_init(id) {
                    // We normally would open this stream, so update our
                    // next-send-id record.
                    shared.send.maybe_reset_next_stream_id(id);
                } else {
                    // We normally would recv this stream, so update our
                    // next-recv-id record.
                    self.actions.recv.maybe_reset_next_stream_id(id);
                }

                let stream = Stream::new(id, 0, 0);

                let key = e.insert(stream);
                shared.counts.inc_num_wired_streams();
                key
            }
        };

        let stream = self.store.resolve(key);
        let mut send_buffer = send_buffer.inner.lock().unwrap();
        let send_buffer = &mut *send_buffer;
        self.actions.send_reset(
            stream,
            reason,
            Initiator::Library,
            &shared.counts,
            &mut self.counts,
            send_buffer,
            &shared.conn_task,
        )
    }
}

impl<B, P> Streams<B, P>
where
    P: Peer,
{
    pub fn as_dyn(&mut self) -> DynStreams<'_, B> {
        let Self {
            inner,
            send_buffer,
            shared,
            _p,
        } = self;
        DynStreams {
            inner,
            send_buffer,
            shared,
            peer: P::r#dyn(),
        }
    }

    /// This function is safe to call multiple times.
    pub fn recv_eof(&mut self, clear_pending_accept: bool) -> Result<(), ()> {
        self.as_dyn().recv_eof(clear_pending_accept)
    }

    pub(crate) fn max_send_streams(&self) -> usize {
        self.inner.counts.max_send_streams(&self.shared.counts)
    }

    pub(crate) fn max_recv_streams(&self) -> usize {
        self.inner.counts.max_recv_streams(&self.shared.counts)
    }

    pub fn has_streams(&self) -> bool {
        self.inner.counts.has_streams(&self.shared.counts)
    }

    pub fn has_streams_or_other_references(&self) -> bool {
        self.inner.counts.has_streams(&self.shared.counts) || Arc::strong_count(&self.shared) > 1
    }

    #[cfg(feature = "unstable")]
    pub fn num_wired_streams(&self) -> usize {
        self.shared.counts.num_wired_streams()
    }
}

impl<B, P> Drop for Streams<B, P>
where
    P: Peer,
{
    fn drop(&mut self) {
        if Arc::strong_count(&self.shared) == 2 {
            self.shared.conn_task.wake();
        }
    }
}

// ===== impl Injector =====

impl<B> Injector<B>
where
    B: Buf,
{
    pub fn poll_pending_open(
        &mut self,
        cx: &Context,
        pending: Option<&OpaqueStreamRef>,
    ) -> Poll<Result<(), crate::Error>> {
        self.shared.ensure_no_conn_error()?;
        self.shared.send.ensure_next_stream_id()?;

        if let Some(pending) = pending {
            let is_pending_open = pending.stream.is_pending_open();
            tracing::trace!("poll_pending_open; stream = {:?}", is_pending_open);
            if is_pending_open {
                pending.stream.wait_send(cx);
                return Poll::Pending;
            }
        }
        Poll::Ready(Ok(()))
    }

    pub fn enqueue_request(
        &mut self,
        mut request: Request<()>,
        end_of_stream: bool,
        pending: Option<&OpaqueStreamRef>,
    ) -> Result<StreamRef<B>, SendError> {
        // The `pending` argument is provided by the `Client`, and holds
        // a store `Key` of a `Stream` that may have been not been opened
        // yet.
        //
        // If that stream is still pending, the Client isn't allowed to
        // queue up another pending stream. They should use `poll_ready`.
        if let Some(stream) = pending {
            if stream.stream.is_pending_open() {
                return Err(UserError::Rejected.into());
            }
        }
        let pending = AssertNoPendingRequest;

        let protocol = request.extensions_mut().remove::<Protocol>();

        // Clear before taking lock, incase extensions contain a StreamRef.
        request.extensions_mut().clear();

        self.shared.ensure_no_conn_error()?;
        self.shared.send.ensure_next_stream_id()?;

        let stream = self.prepare_request(
            request,
            protocol,
            end_of_stream,
            pending,
            self.shared.send.init_window_sz(),
            self.shared.recv.init_window_sz(),
        )?;

        Ok(stream)
    }

    fn prepare_request(
        &mut self,
        request: Request<()>,
        protocol: Option<Protocol>,
        end_of_stream: bool,
        _: AssertNoPendingRequest,
        init_send_window: WindowSize,
        init_recv_window: WindowSize,
    ) -> Result<StreamRef<B>, SendError> {
        // TODO: There is a hazard with assigning a stream ID before the
        // prioritize layer. If prioritization reorders new streams, this
        // implicitly closes the earlier stream IDs.
        //
        // See: hyperium/h2#11

        let stream_id = self.shared.send.open()?;
        let stream = stream::Shared::new(stream_id, init_send_window, init_recv_window);

        // Convert the message
        let headers =
            client::Peer::convert_send_message(stream_id, request, protocol, end_of_stream)?;

        Send::check_headers(headers.fields())?;
        {
            let queued_request_headers = self.shared.pending_request_headers_count();
            let is_pending_open = self.shared.counts.num_send_streams() + queued_request_headers
                >= self.shared.counts.max_send_streams();
            stream.state().state.send_open(end_of_stream)?;
            stream.send().is_pending_open = is_pending_open;
        }

        let stream_ref = StreamRef {
            opaque: OpaqueStreamRef::from_shared(self.shared.clone(), stream.clone()),
            send_buffer: self.send_buffer.clone(),
        };

        self.shared
            .push_op(ConnOp::RequestHeaders { frame: headers }, stream);

        Ok(stream_ref)
    }

    pub fn is_extended_connect_protocol_enabled(&self) -> bool {
        self.shared.send.is_extended_connect_protocol_enabled()
    }

    pub fn current_max_send_streams(&self) -> usize {
        self.shared.counts.max_send_streams()
    }

    pub fn current_max_recv_streams(&self) -> usize {
        self.shared.counts.current_max_recv_streams()
    }

    #[cfg(feature = "unstable")]
    pub fn num_active_streams(&self) -> usize {
        self.shared.counts.num_active_streams()
    }

    #[cfg(feature = "unstable")]
    pub fn num_wired_streams(&self) -> usize {
        self.shared.counts.num_wired_streams()
    }
}

// no derive because we don't need B and P to be Clone.
impl<B> Clone for Injector<B> {
    fn clone(&self) -> Self {
        Injector {
            send_buffer: self.send_buffer.clone(),
            shared: self.shared.clone(),
        }
    }
}

// ===== impl StreamRef =====

impl<B> StreamRef<B> {
    pub fn send_data(&mut self, data: B, end_stream: bool) -> Result<(), UserError>
    where
        B: Buf,
    {
        let (frame, prepared) = {
            let mut frame = frame::Data::new(self.opaque.stream.id, data);
            frame.set_end_stream(end_stream);
            let prepared = Send::prepare_data(&frame, &self.opaque.stream)?;
            (frame, prepared)
        };

        self.send_buffer
            .push_pending_op_data(frame.into(), &self.opaque.stream);

        self.opaque
            .shared
            .push_op(ConnOp::Data { prepared }, self.opaque.stream.clone());

        Ok(())
    }

    pub fn send_trailers(&mut self, trailers: HeaderMap) -> Result<(), UserError> {
        if !self.opaque.stream.state().state.is_send_streaming() {
            return Err(UserError::UnexpectedFrameType);
        }
        self.opaque.stream.state().state.send_close();
        let frame = frame::Headers::trailers(self.opaque.stream.id, trailers);

        self.opaque
            .shared
            .push_op(ConnOp::Trailers { frame }, self.opaque.stream.clone());

        Ok(())
    }

    pub fn send_reset(&mut self, reason: Reason) {
        let has_queued_request_headers = self
            .opaque
            .shared
            .has_queued_request_headers_for(self.opaque.stream.id);
        let prepared = Send::prepare_reset(
            &self.opaque.stream,
            self.is_pending_open() || has_queued_request_headers,
            reason,
            Initiator::User,
        );

        if let Some(prepared) = prepared {
            self.opaque.shared.push_op(
                ConnOp::Reset { reason, prepared },
                self.opaque.stream.clone(),
            );
        }
    }

    pub fn send_informational_headers(&mut self, frame: frame::Headers) -> Result<(), UserError> {
        debug_assert!(
            frame.is_informational(),
            "Frame must be informational after conversion from informational response"
        );

        if frame.is_end_stream() {
            return Err(UserError::UnexpectedFrameType);
        }

        Send::check_headers(frame.fields())?;

        self.opaque.shared.push_op(
            ConnOp::InformationalHeaders { frame },
            self.opaque.stream.clone(),
        );

        Ok(())
    }

    pub fn send_response(
        &mut self,
        mut response: Response<()>,
        end_of_stream: bool,
    ) -> Result<(), UserError> {
        // Clear before taking lock, incase extensions contain a StreamRef.
        response.extensions_mut().clear();
        let frame =
            server::Peer::convert_send_message(self.opaque.stream.id, response, end_of_stream);
        Send::check_headers(frame.fields())?;
        self.opaque.stream.state().state.send_open(end_of_stream)?;

        self.opaque.shared.push_op(
            ConnOp::ResponseHeaders { frame },
            self.opaque.stream.clone(),
        );

        Ok(())
    }

    pub fn send_push_promise(
        &mut self,
        mut request: Request<()>,
    ) -> Result<StreamRef<B>, UserError> {
        // Clear before taking lock, incase extensions contain a StreamRef.
        request.extensions_mut().clear();
        let promised_id = self.opaque.shared.send.reserve_local()?;

        if !self.opaque.shared.send.is_push_enabled() {
            return Err(UserError::PeerDisabledServerPush);
        }

        let frame =
            crate::server::Peer::convert_push_message(self.opaque.stream.id, promised_id, request)?;
        Send::check_headers(frame.fields())?;

        let child_stream = stream::Shared::new(
            promised_id,
            self.opaque.shared.send.init_window_sz(),
            self.opaque.shared.recv.init_window_sz(),
        );
        {
            child_stream.state().state.reserve_local()?;
            child_stream.state().is_pending_push = true;
        }

        self.opaque.shared.push_op(
            ConnOp::PushPromise {
                frame,
                promised: child_stream.clone(),
            },
            self.opaque.stream.clone(),
        );

        let opaque = OpaqueStreamRef::from_shared(self.opaque.shared.clone(), child_stream);

        Ok(StreamRef {
            opaque,
            send_buffer: self.send_buffer.clone(),
        })
    }

    /// Called by the server after the stream is accepted. Given that clients
    /// initialize streams by sending HEADERS, the request will always be
    /// available.
    ///
    /// # Panics
    ///
    /// This function panics if the request isn't present.
    pub fn take_request(&self) -> Request<()> {
        super::recv::take_request(&self.opaque.stream, &self.opaque.shared.recv)
    }

    /// Called by a client to see if the current stream is pending open
    pub fn is_pending_open(&self) -> bool {
        self.opaque.stream.is_pending_open()
    }

    /// Request capacity to send data
    pub fn reserve_capacity(&mut self, capacity: WindowSize) {
        let prepared = send::prepare_reserve_capacity(capacity, &self.opaque.stream);

        if let Some(prepared) = prepared {
            self.opaque.shared.push_op(
                ConnOp::ReserveCapacity { prepared },
                self.opaque.stream.clone(),
            );
        }
    }

    /// Returns the stream's current send capacity.
    pub fn capacity(&self) -> WindowSize {
        self.opaque.shared.send.capacity(&self.opaque.stream)
    }

    /// Request to be notified when the stream's capacity increases
    pub fn poll_capacity(&mut self, cx: &Context) -> Poll<Option<Result<WindowSize, UserError>>> {
        self.opaque
            .shared
            .send
            .poll_capacity(cx, &self.opaque.stream)
    }

    /// Request to be notified for if a `RST_STREAM` is received for this stream.
    pub(crate) fn poll_reset(
        &mut self,
        cx: &Context,
        mode: proto::PollReset,
    ) -> Poll<Result<Reason, crate::Error>> {
        super::send::poll_reset(cx, &self.opaque.stream, mode)
    }

    pub fn clone_to_opaque(&self) -> OpaqueStreamRef {
        self.opaque.clone()
    }

    pub fn stream_id(&self) -> StreamId {
        self.opaque.stream_id()
    }
}

impl<B> Clone for StreamRef<B> {
    fn clone(&self) -> Self {
        StreamRef {
            opaque: self.opaque.clone(),
            send_buffer: self.send_buffer.clone(),
        }
    }
}

// ===== impl OpaqueStreamRef =====

impl OpaqueStreamRef {
    fn new(shared: Arc<Shared>, stream: &mut store::Ptr) -> OpaqueStreamRef {
        OpaqueStreamRef::from_shared(shared, stream.clone_shared())
    }

    fn from_shared(shared: Arc<Shared>, stream: Arc<stream::Shared>) -> OpaqueStreamRef {
        stream.ref_inc();
        OpaqueStreamRef { shared, stream }
    }
    /// Called by a client to check for a received response.
    pub fn poll_response(&mut self, cx: &Context) -> Poll<Result<Response<()>, proto::Error>> {
        super::recv::poll_response(cx, &self.stream, &self.shared.recv)
    }

    /// Called by a client to check for informational responses (1xx status codes)
    pub fn poll_informational(
        &mut self,
        cx: &Context,
    ) -> Poll<Option<Result<Response<()>, proto::Error>>> {
        super::recv::poll_informational(cx, &self.stream, &self.shared.recv)
    }
    /// Called by a client to check for a pushed request.
    pub fn poll_pushed(
        &mut self,
        cx: &Context,
    ) -> Poll<Option<Result<(Request<()>, OpaqueStreamRef), proto::Error>>> {
        super::recv::poll_pushed(cx, &self.stream, &self.shared.recv).map_ok(|(h, stream)| {
            let opaque_ref = OpaqueStreamRef::from_shared(self.shared.clone(), stream);
            (h, opaque_ref)
        })
    }

    pub fn is_end_stream(&self) -> bool {
        super::recv::is_end_stream(&self.stream)
    }

    pub fn poll_data(&mut self, cx: &Context) -> Poll<Option<Result<Bytes, proto::Error>>> {
        super::recv::poll_data(cx, &self.stream, &self.shared.recv)
    }

    pub fn poll_trailers(&mut self, cx: &Context) -> Poll<Option<Result<HeaderMap, proto::Error>>> {
        super::recv::poll_trailers(cx, &self.stream, &self.shared.recv)
    }

    pub(crate) fn available_recv_capacity(&self) -> isize {
        self.stream.recv().recv_flow.available().into()
    }

    pub(crate) fn used_recv_capacity(&self) -> WindowSize {
        self.stream.recv().in_flight_recv_data
    }

    /// Releases recv capacity back to the peer. This may result in sending
    /// WINDOW_UPDATE frames on both the stream and connection.
    pub fn release_capacity(&mut self, capacity: WindowSize) -> Result<(), UserError> {
        super::recv::release_capacity_local(capacity, &self.stream)?;

        self.shared
            .push_op(ConnOp::ReleaseCapacity { capacity }, self.stream.clone());

        Ok(())
    }

    /// Clear the receive queue and set the status to no longer receive data frames.
    pub(crate) fn clear_recv_buffer(&mut self) {
        self.stream.recv().is_recv = false;
        self.shared.recv.clear(&self.stream);
    }

    pub fn stream_id(&self) -> StreamId {
        self.stream.id
    }
}

impl fmt::Debug for OpaqueStreamRef {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        let state = self.stream.state();
        fmt.debug_struct("OpaqueStreamRef")
            .field("stream_id", &self.stream.id)
            .field("ref_count", &state.ref_count)
            .finish()
    }
}

impl Clone for OpaqueStreamRef {
    fn clone(&self) -> Self {
        self.stream.ref_inc();

        OpaqueStreamRef {
            shared: self.shared.clone(),
            stream: self.stream.clone(),
        }
    }
}

impl Drop for OpaqueStreamRef {
    fn drop(&mut self) {
        drop_stream_ref(&self.shared, &self.stream);
    }
}

// TODO: Move back in fn above
fn drop_stream_ref(shared: &Shared, stream: &Arc<stream::Shared>) {
    tracing::trace!("drop_stream_ref; stream={:?}", stream);

    // Keep the stream alive until the deferred cleanup op runs.
    stream.ops_inc();

    // decrement the stream's ref count by 1.
    stream.ref_dec();

    shared.enqueue_op(ConnOp::DropStreamRef, stream.clone());
}

fn release_queued_send_ref(
    actions: &mut Actions,
    counts: &mut Counts,
    shared: &Shared,
    counts_shared: &counts::Shared,
    store: &mut Store,
    stream: &Arc<stream::Shared>,
) {
    let Some(stream) = store.find_mut_even_if_unlinked(&stream) else {
        return;
    };

    tracing::trace!("release_queued_send_ref; stream={:?}", stream);

    let is_closed = stream.is_closed();
    let is_unreferenced_and_closed = {
        let shared = stream.shared();
        shared.ref_count == 0 && is_closed
    };
    if is_unreferenced_and_closed {
        shared.conn_task.wake();
    }

    counts.transition(counts_shared, stream, |counts, stream| {
        maybe_cancel(stream, actions, &shared.counts, counts, &shared.conn_task);

        if stream.shared().ref_count == 0 {
            actions
                .recv
                .release_closed_capacity(stream, &shared.conn_task, &shared.recv);

            while let Some(promise) = shared.recv.pop_pushed(stream.shared_ref()) {
                if let Some(promise) = stream.store_mut().find_mut(&promise.id) {
                    counts.transition(counts_shared, promise, |counts, stream| {
                        maybe_cancel(stream, actions, counts_shared, counts, &shared.conn_task);
                    });
                }
            }
        }
    });
}

fn maybe_cancel(
    stream: &mut store::Ptr,
    actions: &mut Actions,
    counts_shared: &counts::Shared,
    counts: &mut Counts,
    conn_task: &ConnWaker,
) {
    if stream.is_canceled_interest() {
        // Server is allowed to early respond without fully consuming the client input stream
        // But per the RFC, must send a RST_STREAM(NO_ERROR) in such cases. https://www.rfc-editor.org/rfc/rfc7540#section-8.1
        // Some other http2 implementation may interpret other error code as fatal if not respected (i.e: nginx https://trac.nginx.org/nginx/ticket/2376)
        let reason = if {
            let shared = stream.shared();
            counts.peer().is_server()
                && shared.state.is_send_closed()
                && shared.state.is_recv_streaming()
        } {
            Reason::NO_ERROR
        } else {
            Reason::CANCEL
        };

        actions
            .send
            .schedule_implicit_reset(stream, reason, counts_shared, counts, conn_task);
        actions.recv.enqueue_reset_expiration(stream, counts);
    }
}

// ===== impl SendBuffer =====

impl<B> SendBuffer<B> {
    fn new() -> Self {
        let inner = Mutex::new(Buffer::new());
        SendBuffer { inner }
    }

    pub fn is_empty(&self) -> bool {
        let buf = self.inner.lock().unwrap();
        buf.is_empty()
    }

    fn push_pending_op_data(&self, frame: Frame<B>, stream: &stream::Shared) {
        let mut send_frames = self.inner.lock().unwrap();
        stream
            .send()
            .pending_op_data
            .push_back(&mut send_frames, frame);
    }
}

impl Shared {
    fn push_op(&self, op: ConnOp, stream: Arc<stream::Shared>) {
        stream.ops_inc();
        self.enqueue_op(op, stream);
    }

    fn enqueue_op(&self, op: ConnOp, stream: Arc<stream::Shared>) {
        if matches!(&op, ConnOp::RequestHeaders { .. }) {
            self.queued_request_headers.fetch_add(1, Ordering::Relaxed);
        }
        let mut pending_ops = self.pending_ops.lock().unwrap();
        pending_ops.push_back(QueuedOp { stream, op });

        self.conn_task.wake();
    }

    fn pending_request_headers_count(&self) -> usize {
        self.queued_request_headers.load(Ordering::Relaxed)
    }

    fn has_queued_request_headers_for(&self, id: StreamId) -> bool {
        self.pending_ops
            .lock()
            .unwrap()
            .has_queued_request_headers_for(id)
    }

    fn has_queued_frame_for(&self, id: StreamId) -> bool {
        self.pending_ops.lock().unwrap().has_queued_frame_for(id)
    }

    fn ensure_no_conn_error(&self) -> Result<(), proto::Error> {
        if let Some(ref err) = *self.conn_error.read().unwrap() {
            Err(err.clone())
        } else {
            Ok(())
        }
    }
}

// ===== impl Actions =====

impl Actions {
    fn send_reset<B>(
        &mut self,
        stream: store::Ptr,
        reason: Reason,
        initiator: Initiator,
        counts_shared: &counts::Shared,
        counts: &mut Counts,
        send_buffer: &mut Buffer<Frame<B>>,
        conn_task: &ConnWaker,
    ) -> Result<(), crate::proto::error::GoAway> {
        counts.transition(counts_shared, stream, |counts, stream| {
            if initiator.is_library() {
                if counts.can_inc_num_local_error_resets() {
                    counts.inc_num_local_error_resets();
                } else {
                    tracing::warn!(
                        "locally-reset streams reached limit ({:?})",
                        counts.max_local_error_resets().unwrap(),
                    );
                    return Err(crate::proto::error::GoAway {
                        reason: Reason::ENHANCE_YOUR_CALM,
                        debug_data: "too_many_internal_resets".into(),
                    });
                }
            }

            self.send.send_reset(
                reason,
                initiator,
                send_buffer,
                stream,
                counts_shared,
                counts,
                conn_task,
            );
            self.recv.enqueue_reset_expiration(stream, counts);
            // if a RecvStream is parked, ensure it's notified
            stream.notify_recv();

            Ok(())
        })
    }

    fn reset_on_recv_stream_err<B>(
        &mut self,
        buffer: &mut Buffer<Frame<B>>,
        stream: &mut store::Ptr,
        counts_shared: &counts::Shared,
        counts: &mut Counts,
        res: Result<(), Error>,
        conn_task: &ConnWaker,
    ) -> Result<(), Error> {
        if let Err(Error::Reset(stream_id, reason, initiator)) = res {
            debug_assert_eq!(stream_id, stream.id);

            if counts.can_inc_num_local_error_resets() {
                counts.inc_num_local_error_resets();

                // Reset the stream.
                self.send.send_reset(
                    reason,
                    initiator,
                    buffer,
                    stream,
                    counts_shared,
                    counts,
                    conn_task,
                );
                self.recv.enqueue_reset_expiration(stream, counts);
                // if a RecvStream is parked, ensure it's notified
                stream.notify_recv();
                Ok(())
            } else {
                tracing::warn!(
                    "reset_on_recv_stream_err; locally-reset streams reached limit ({:?})",
                    counts.max_local_error_resets().unwrap(),
                );
                Err(Error::library_go_away_data(
                    Reason::ENHANCE_YOUR_CALM,
                    "too_many_internal_resets",
                ))
            }
        } else {
            res
        }
    }

    fn ensure_not_idle(
        &self,
        peer: peer::Dyn,
        shared: &Shared,
        id: StreamId,
    ) -> Result<(), Reason> {
        if peer.is_local_init(id) {
            shared.send.ensure_not_idle(id)
        } else {
            self.recv.ensure_not_idle(id)
        }
    }

    /// Check if we possibly could have processed and since forgotten this stream.
    ///
    /// If we send a RST_STREAM for a stream, we will eventually "forget" about
    /// the stream to free up memory. It's possible that the remote peer had
    /// frames in-flight, and by the time we receive them, our own state is
    /// gone. We *could* tear everything down by sending a GOAWAY, but it
    /// is more likely to be latency/memory constraints that caused this,
    /// and not a bad actor. So be less catastrophic, the spec allows
    /// us to send another RST_STREAM of STREAM_CLOSED.
    fn may_have_forgotten_stream(&self, peer: peer::Dyn, shared: &Shared, id: StreamId) -> bool {
        if id.is_zero() {
            return false;
        }
        if peer.is_local_init(id) {
            shared.send.may_have_created_stream(id)
        } else {
            self.recv.may_have_created_stream(id)
        }
    }

    fn clear_queues(
        &mut self,
        clear_pending_accept: bool,
        counts_shared: &counts::Shared,
        store: &mut Store,
        counts: &mut Counts,
    ) {
        self.recv
            .clear_queues(clear_pending_accept, store, counts_shared, counts);
        self.send.clear_queues(counts_shared, store, counts);
    }
}

// ===== impl ConnWaker =====

impl ConnWaker {
    fn new() -> Self {
        ConnWaker(atomic_waker::AtomicWaker::new())
    }

    pub(super) fn register(&self, waker: &Waker) {
        self.0.register(waker);
    }

    pub(super) fn wake(&self) {
        self.0.wake();
    }
}
