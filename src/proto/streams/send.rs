use super::counts;
use super::stream;
use super::streams::ConnWaker;
use super::{
    store, Buffer, Codec, Config, Counts, Frame, Prioritize, Prioritized, Store, StreamId,
    StreamIdOverflow, WindowSize,
};
use crate::codec::UserError;
use crate::frame::{self, Reason};
use crate::proto::{self, Error, Initiator, MAX_WINDOW_SIZE};

use bytes::Buf;
use tokio::io::AsyncWrite;

use std::cmp::{self, Ordering};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering as AtomicOrdering};
use std::task::{Context, Poll};

#[derive(Debug, Clone, Copy)]
pub(super) struct PreparedSendData {
    pub(super) needs_capacity: bool,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PreparedSendReset {
    pub(super) clear_queue: bool,
    pub(super) send_explicit_reset: bool,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum PreparedReserveCapacity {
    AssignConnection(WindowSize),
    TryAssign,
}

/// Manages state transitions related to outbound frames.
#[derive(Debug)]
pub(super) struct Send {
    /// Any streams with a higher ID are ignored.
    ///
    /// This starts as MAX, but is lowered when a GOAWAY is received.
    ///
    /// > After sending a GOAWAY frame, the sender can discard frames for
    /// > streams initiated by the receiver with identifiers higher than
    /// > the identified last stream.
    max_stream_id: StreamId,

    /// Prioritization layer
    prioritize: Prioritize,
}

#[derive(Debug)]
pub(super) struct Shared {
    /// Initial window size of locally initiated streams
    init_window_sz: AtomicU32,

    is_push_enabled: AtomicBool,

    /// If extended connect protocol is enabled.
    is_extended_connect_protocol_enabled: AtomicBool,

    /// The maximum amount of bytes a stream should buffer.
    max_buffer_size: usize,

    /// Stream identifier to use for next initialized stream.
    next_stream_id: NextStreamId,
}

#[derive(Debug)]
pub(super) struct NextStreamId(AtomicU64);

/// A value to detect which public API has called `poll_reset`.
#[derive(Debug)]
pub(crate) enum PollReset {
    AwaitingHeaders,
    Streaming,
}

impl Send {
    /// Create a new `Send`
    pub fn new(config: &Config) -> (Self, Shared) {
        (
            Send {
                max_stream_id: StreamId::MAX,
                prioritize: Prioritize::new(config),
            },
            Shared {
                init_window_sz: AtomicU32::new(config.remote_init_window_sz),
                is_push_enabled: AtomicBool::new(true),
                is_extended_connect_protocol_enabled: AtomicBool::new(false),
                max_buffer_size: config.local_max_buffer_size,
                next_stream_id: NextStreamId::new(config.local_next_stream_id),
            },
        )
    }

    pub(super) fn check_headers(fields: &http::HeaderMap) -> Result<(), UserError> {
        // 8.1.2.2. Connection-Specific Header Fields
        if fields.contains_key(http::header::CONNECTION)
            || fields.contains_key(http::header::TRANSFER_ENCODING)
            || fields.contains_key(http::header::UPGRADE)
            || fields.contains_key("keep-alive")
            || fields.contains_key("proxy-connection")
        {
            tracing::debug!("illegal connection-specific headers found");
            return Err(UserError::MalformedHeaders);
        } else if let Some(te) = fields.get(http::header::TE) {
            if te != "trailers" {
                tracing::debug!("illegal connection-specific headers found");
                return Err(UserError::MalformedHeaders);
            }
        }
        Ok(())
    }

    pub(super) fn send_push_promise_frame<B>(
        &mut self,
        shared: &Shared,
        frame: frame::PushPromise,
        buffer: &mut Buffer<Frame<B>>,
        stream: &mut store::Ptr,
        conn_task: &ConnWaker,
    ) {
        tracing::trace!(
            "send_push_promise; frame={:?}; init_window={:?}",
            frame,
            shared.init_window_sz()
        );

        // Queue the frame for sending
        self.prioritize
            .queue_frame(frame.into(), buffer, stream, conn_task);
    }

    pub fn send_headers<B>(
        &mut self,
        shared: &Shared,
        frame: frame::Headers,
        buffer: &mut Buffer<Frame<B>>,
        stream: &mut store::Ptr,
        counts: &mut Counts,
        conn_task: &ConnWaker,
    ) -> Result<(), UserError> {
        tracing::trace!(
            "send_headers; frame={:?}; init_window={:?}",
            frame,
            shared.init_window_sz()
        );

        Self::check_headers(frame.fields())?;
        stream.shared().state.send_open(frame.is_end_stream())?;
        self.send_headers_after_prepare(shared, frame, buffer, stream, counts, conn_task);

        Ok(())
    }

    pub(super) fn send_headers_after_prepare<B>(
        &mut self,
        _shared: &Shared,
        frame: frame::Headers,
        buffer: &mut Buffer<Frame<B>>,
        stream: &mut store::Ptr,
        counts: &mut Counts,
        conn_task: &ConnWaker,
    ) {
        let mut pending_open = false;
        if counts.peer().is_local_init(frame.stream_id()) && !stream.shared().is_pending_push {
            self.prioritize.queue_open(stream);
            pending_open = true;
        }

        // Queue the frame for sending
        //
        // New local streams may be queued both for opening and for frame send.
        self.prioritize
            .queue_frame(frame.into(), buffer, stream, conn_task);

        // Need to notify the connection when pushing onto pending_open since
        // queue_frame only notifies for pending_send.
        if pending_open {
            conn_task.wake();
        }
    }

    pub(super) fn send_headers_frame<B>(
        &mut self,
        shared: &Shared,
        frame: frame::Headers,
        buffer: &mut Buffer<Frame<B>>,
        stream: &mut store::Ptr,
        conn_task: &ConnWaker,
    ) {
        tracing::trace!(
            "send_headers; frame={:?}; init_window={:?}",
            frame,
            shared.init_window_sz()
        );

        self.prioritize
            .queue_frame(frame.into(), buffer, stream, conn_task);
    }

    pub(super) fn send_interim_informational_headers_frame<B>(
        &mut self,
        frame: frame::Headers,
        buffer: &mut Buffer<Frame<B>>,
        stream: &mut store::Ptr,
        conn_task: &ConnWaker,
    ) {
        tracing::trace!(
            "send_interim_informational_headers; frame={:?}; stream_id={:?}",
            frame,
            frame.stream_id()
        );

        self.prioritize
            .queue_frame(frame.into(), buffer, stream, conn_task);
    }

    /// Send an explicit RST_STREAM frame
    pub fn send_reset<B>(
        &mut self,
        reason: Reason,
        initiator: Initiator,
        buffer: &mut Buffer<Frame<B>>,
        stream: &mut store::Ptr,
        counts_shared: &counts::Shared,
        counts: &mut Counts,
        conn_task: &ConnWaker,
    ) {
        let is_pending_open = stream.send().is_pending_open;
        if let Some(prepared) =
            Self::prepare_reset(stream.shared_ref(), is_pending_open, reason, initiator)
        {
            self.send_prepared_reset(
                reason,
                prepared,
                buffer,
                stream,
                counts_shared,
                counts,
                conn_task,
            );
        }
    }

    pub fn schedule_implicit_reset(
        &mut self,
        stream: &mut store::Ptr,
        reason: Reason,
        counts_shared: &counts::Shared,
        counts: &mut Counts,
        conn_task: &ConnWaker,
    ) {
        if stream.shared().state.is_closed() {
            // Stream is already closed, nothing more to do
            return;
        }

        stream.shared().state.set_scheduled_reset(reason);

        self.prioritize
            .reclaim_reserved_capacity(stream, counts_shared, counts);
        self.prioritize.schedule_send(stream, conn_task);
    }

    pub(super) fn prepare_data<B>(
        frame: &frame::Data<B>,
        stream: &stream::Shared,
    ) -> Result<PreparedSendData, UserError>
    where
        B: Buf,
    {
        use crate::codec::UserError::*;

        let sz = frame.payload().remaining();

        if sz > MAX_WINDOW_SIZE as usize {
            return Err(UserError::PayloadTooBig);
        }

        let sz = sz as WindowSize;

        {
            let state = stream.state();
            if !state.state.is_send_streaming() {
                if state.state.is_closed() {
                    return Err(InactiveStreamId);
                } else {
                    return Err(UnexpectedFrameType);
                }
            }
        }

        let needs_capacity = {
            let mut send = stream.send();

            send.buffered_send_data += sz as usize;

            let span =
                tracing::trace_span!("send_data", sz, requested = send.requested_send_capacity);
            let _e = span.enter();
            tracing::trace!(buffered = send.buffered_send_data);

            let needs_capacity = (send.requested_send_capacity as usize) < send.buffered_send_data;
            if needs_capacity {
                send.requested_send_capacity =
                    cmp::min(send.buffered_send_data, WindowSize::MAX as usize) as WindowSize;
            }

            needs_capacity
        };

        if frame.is_end_stream() {
            stream.state().state.send_close();
        }

        Ok(PreparedSendData { needs_capacity })
    }

    pub(super) fn send_prepared_data<B>(
        &mut self,
        frame: frame::Data<B>,
        prepared: PreparedSendData,
        buffer: &mut Buffer<Frame<B>>,
        stream: &mut store::Ptr,
        counts_shared: &counts::Shared,
        counts: &mut Counts,
        conn_task: &ConnWaker,
    ) where
        B: Buf,
    {
        if prepared.needs_capacity {
            self.prioritize.try_assign_capacity(stream);
        }

        if frame.is_end_stream() {
            self.reserve_capacity(0, stream, counts_shared, counts);
        }

        let (available, buffered_send_data) = {
            let send = stream.send();
            (send.send_flow.available(), send.buffered_send_data)
        };

        tracing::trace!(available = %available, buffered = buffered_send_data);

        if available > 0 || buffered_send_data == 0 {
            self.prioritize
                .queue_frame(frame.into(), buffer, stream, conn_task);
        } else {
            stream.send().pending_send.push_back(buffer, frame.into());
        }
    }

    pub(super) fn send_trailers_frame<B>(
        &mut self,
        frame: frame::Headers,
        buffer: &mut Buffer<Frame<B>>,
        stream: &mut store::Ptr,
        counts_shared: &counts::Shared,
        counts: &mut Counts,
        conn_task: &ConnWaker,
    ) {
        tracing::trace!("send_trailers -- queuing; frame={:?}", frame);
        self.prioritize
            .queue_frame(frame.into(), buffer, stream, conn_task);

        // Release any excess capacity
        self.prioritize
            .reserve_capacity(0, stream, counts_shared, counts);
    }

    pub(super) fn prepare_reset(
        stream: &stream::Shared,
        is_pending_open: bool,
        reason: Reason,
        initiator: Initiator,
    ) -> Option<PreparedSendReset> {
        let (is_reset, is_closed, is_empty) = {
            let state = stream.state();
            let send = stream.send();
            (
                state.state.is_reset(),
                state.state.is_closed(),
                send.pending_send.is_empty() && send.pending_op_data.is_empty(),
            )
        };
        let debug_state = stream.state().state.clone();
        let stream_id = stream.id;

        tracing::trace!(
            "send_reset(..., reason={:?}, initiator={:?}, stream={:?}, ..., \
             is_reset={:?}; is_closed={:?}; send_queues_empty={:?}; \
             state={:?} \
             ",
            reason,
            initiator,
            stream_id,
            is_reset,
            is_closed,
            is_empty,
            debug_state
        );

        if is_reset {
            tracing::trace!(
                " -> not sending RST_STREAM ({:?} is already reset)",
                stream_id
            );
            return None;
        }

        stream.set_reset(reason, initiator);

        if is_closed && is_empty {
            tracing::trace!(
                " -> not sending explicit RST_STREAM ({:?} was closed \
                 and send queue was flushed)",
                stream_id
            );
            return Some(PreparedSendReset {
                clear_queue: false,
                send_explicit_reset: false,
            });
        }

        Some(PreparedSendReset {
            clear_queue: !is_pending_open,
            send_explicit_reset: true,
        })
    }

    pub(super) fn send_prepared_reset<B>(
        &mut self,
        reason: Reason,
        prepared: PreparedSendReset,
        buffer: &mut Buffer<Frame<B>>,
        stream: &mut store::Ptr,
        counts_shared: &counts::Shared,
        counts: &mut Counts,
        conn_task: &ConnWaker,
    ) {
        if !prepared.send_explicit_reset {
            return;
        }

        if prepared.clear_queue {
            self.prioritize.clear_queue(buffer, stream);
        }

        let frame = frame::Reset::new(stream.id, reason);

        tracing::trace!("send_reset -- queueing; frame={:?}", frame);
        self.prioritize
            .queue_frame(frame.into(), buffer, stream, conn_task);
        self.prioritize
            .reclaim_all_capacity(stream, counts_shared, counts);
    }

    pub fn poll_complete<T, B>(
        &mut self,
        cx: &mut Context,
        buffer: &mut Buffer<Frame<B>>,
        store: &mut Store,
        counts_shared: &counts::Shared,
        counts: &mut Counts,
        dst: &mut Codec<T, Prioritized<B>>,
    ) -> Poll<io::Result<()>>
    where
        T: AsyncWrite + Unpin,
        B: Buf,
    {
        self.prioritize
            .poll_complete(cx, buffer, store, counts_shared, counts, dst)
    }

    /// Request capacity to send data
    pub fn reserve_capacity(
        &mut self,
        capacity: WindowSize,
        stream: &mut store::Ptr,
        counts_shared: &counts::Shared,
        counts: &mut Counts,
    ) {
        self.prioritize
            .reserve_capacity(capacity, stream, counts_shared, counts)
    }

    pub fn apply_reserve_capacity(
        &mut self,
        prepared: PreparedReserveCapacity,
        stream: &mut store::Ptr,
        counts_shared: &counts::Shared,
        counts: &mut Counts,
    ) {
        match prepared {
            PreparedReserveCapacity::AssignConnection(diff) => {
                self.prioritize
                    .assign_connection_capacity(diff, stream, counts_shared, counts);
            }
            PreparedReserveCapacity::TryAssign => {
                self.prioritize.try_assign_capacity(stream);
            }
        }
    }

    pub fn recv_connection_window_update(
        &mut self,
        frame: frame::WindowUpdate,
        store: &mut Store,
        counts_shared: &counts::Shared,
        counts: &mut Counts,
    ) -> Result<(), Reason> {
        self.prioritize.recv_connection_window_update(
            frame.size_increment(),
            store,
            counts_shared,
            counts,
        )
    }

    pub fn recv_stream_window_update<B>(
        &mut self,
        sz: WindowSize,
        buffer: &mut Buffer<Frame<B>>,
        stream: &mut store::Ptr,
        counts_shared: &counts::Shared,
        counts: &mut Counts,
        conn_task: &ConnWaker,
    ) -> Result<(), Reason> {
        if let Err(e) = self.prioritize.recv_stream_window_update(sz, stream) {
            tracing::debug!("recv_stream_window_update !!; err={:?}", e);

            self.send_reset(
                Reason::FLOW_CONTROL_ERROR,
                Initiator::Library,
                buffer,
                stream,
                counts_shared,
                counts,
                conn_task,
            );

            return Err(e);
        }

        Ok(())
    }

    pub(super) fn recv_go_away(&mut self, last_stream_id: StreamId) -> Result<(), Error> {
        if last_stream_id > self.max_stream_id {
            // The remote endpoint sent a `GOAWAY` frame indicating a stream
            // that we never sent, or that we have already terminated on account
            // of previous `GOAWAY` frame. In either case, that is illegal.
            // (When sending multiple `GOAWAY`s, "Endpoints MUST NOT increase
            // the value they send in the last stream identifier, since the
            // peers might already have retried unprocessed requests on another
            // connection.")
            proto_err!(conn:
                "recv_go_away: last_stream_id ({:?}) > max_stream_id ({:?})",
                last_stream_id, self.max_stream_id,
            );
            return Err(Error::library_go_away(Reason::PROTOCOL_ERROR));
        }

        self.max_stream_id = last_stream_id;
        Ok(())
    }

    pub(super) fn is_ignored_by_go_away(&self, id: StreamId) -> bool {
        id > self.max_stream_id
    }

    pub fn handle_error<B>(
        &mut self,
        buffer: &mut Buffer<Frame<B>>,
        stream: &mut store::Ptr,
        counts_shared: &counts::Shared,
        counts: &mut Counts,
    ) {
        // Clear all pending outbound frames
        self.prioritize.clear_queue(buffer, stream);
        self.prioritize
            .reclaim_all_capacity(stream, counts_shared, counts);
    }

    pub fn apply_remote_settings<B>(
        &mut self,
        shared: &Shared,
        settings: &frame::Settings,
        buffer: &mut Buffer<Frame<B>>,
        store: &mut Store,
        counts_shared: &counts::Shared,
        counts: &mut Counts,
        conn_task: &ConnWaker,
    ) -> Result<(), Error> {
        if let Some(val) = settings.is_extended_connect_protocol_enabled() {
            shared.set_extended_connect_protocol_enabled(val);
        }

        // Applies an update to the remote endpoint's initial window size.
        //
        // Per RFC 7540 §6.9.2:
        //
        // In addition to changing the flow-control window for streams that are
        // not yet active, a SETTINGS frame can alter the initial flow-control
        // window size for streams with active flow-control windows (that is,
        // streams in the "open" or "half-closed (remote)" state). When the
        // value of SETTINGS_INITIAL_WINDOW_SIZE changes, a receiver MUST adjust
        // the size of all stream flow-control windows that it maintains by the
        // difference between the new value and the old value.
        //
        // A change to `SETTINGS_INITIAL_WINDOW_SIZE` can cause the available
        // space in a flow-control window to become negative. A sender MUST
        // track the negative flow-control window and MUST NOT send new
        // flow-controlled frames until it receives WINDOW_UPDATE frames that
        // cause the flow-control window to become positive.
        if let Some(val) = settings.initial_window_size() {
            let old_val = shared.init_window_sz();
            shared.set_init_window_sz(val);

            match val.cmp(&old_val) {
                Ordering::Less => {
                    // We must decrease the (remote) window on every open stream.
                    let dec = old_val - val;
                    tracing::trace!("decrementing all windows; dec={}", dec);

                    let mut total_reclaimed = 0;
                    store.try_for_each(|stream| {
                        let mut send = stream.send();
                        if stream.shared().state.is_send_closed() && send.buffered_send_data == 0 {
                            tracing::trace!(
                                "skipping send-closed stream; id={:?}; flow={:?}",
                                stream.id,
                                send.send_flow
                            );

                            return Ok(());
                        }

                        tracing::trace!(
                            "decrementing stream window; id={:?}; decr={}; flow={:?}",
                            stream.id,
                            dec,
                            send.send_flow
                        );

                        // TODO: this decrement can underflow based on received frames!
                        send.send_flow
                            .dec_send_window(dec)
                            .map_err(proto::Error::library_go_away)?;

                        // It's possible that decreasing the window causes
                        // `window_size` (the stream-specific window) to fall below
                        // `available` (the portion of the connection-level window
                        // that we have allocated to the stream).
                        // In this case, we should take that excess allocation away
                        // and reassign it to other streams.
                        let window_size = send.send_flow.window_size();
                        let available = send.send_flow.available().as_size();
                        let reclaimed = if available > window_size {
                            // Drop down to `window_size`.
                            let reclaim = available - window_size;
                            send.send_flow
                                .claim_capacity(reclaim)
                                .map_err(proto::Error::library_go_away)?;
                            total_reclaimed += reclaim;
                            reclaim
                        } else {
                            0
                        };

                        tracing::trace!(
                            "decremented stream window; id={:?}; decr={}; reclaimed={}; flow={:?}",
                            stream.id,
                            dec,
                            reclaimed,
                            send.send_flow
                        );

                        // TODO: Should this notify the producer when the capacity
                        // of a stream is reduced? Maybe it should if the capacity
                        // is reduced to zero, allowing the producer to stop work.

                        Ok::<_, proto::Error>(())
                    })?;

                    self.prioritize.assign_connection_capacity(
                        total_reclaimed,
                        store,
                        counts_shared,
                        counts,
                    );
                }
                Ordering::Greater => {
                    let inc = val - old_val;

                    store.try_for_each(|mut stream| {
                        self.recv_stream_window_update(
                            inc,
                            buffer,
                            &mut stream,
                            counts_shared,
                            counts,
                            conn_task,
                        )
                        .map_err(Error::library_go_away)
                    })?;
                }
                Ordering::Equal => (),
            }
        }

        if let Some(val) = settings.is_push_enabled() {
            shared.set_push_enabled(val)
        }

        Ok(())
    }

    pub fn clear_queues(
        &mut self,
        counts_shared: &counts::Shared,
        store: &mut Store,
        counts: &mut Counts,
    ) {
        self.prioritize
            .clear_pending_capacity(counts_shared, store, counts);
        self.prioritize
            .clear_pending_send(counts_shared, store, counts);
        self.prioritize
            .clear_pending_open(counts_shared, store, counts);
    }
}

impl Shared {
    pub fn init_window_sz(&self) -> WindowSize {
        self.init_window_sz.load(AtomicOrdering::Relaxed)
    }

    fn set_init_window_sz(&self, val: WindowSize) {
        self.init_window_sz.store(val, AtomicOrdering::Relaxed);
    }

    pub fn is_push_enabled(&self) -> bool {
        self.is_push_enabled.load(AtomicOrdering::Relaxed)
    }

    fn set_push_enabled(&self, val: bool) {
        self.is_push_enabled.store(val, AtomicOrdering::Relaxed);
    }

    pub fn is_extended_connect_protocol_enabled(&self) -> bool {
        self.is_extended_connect_protocol_enabled
            .load(AtomicOrdering::Relaxed)
    }

    fn set_extended_connect_protocol_enabled(&self, val: bool) {
        self.is_extended_connect_protocol_enabled
            .store(val, AtomicOrdering::Relaxed);
    }

    pub fn poll_capacity(
        &self,
        cx: &Context,
        stream: &stream::Shared,
    ) -> Poll<Option<Result<WindowSize, UserError>>> {
        if !stream.state().state.is_send_streaming() {
            return Poll::Ready(None);
        }

        let mut send = stream.send();
        if !send.send_capacity_inc {
            send.wait_send(cx);
            return Poll::Pending;
        }

        send.send_capacity_inc = false;

        let capacity = send.capacity(self.max_buffer_size);

        // If capacity has been reduced to zero, for example due to a race
        // with a SETTINGS frame, return Pending instead of Ready(Ok(0)).
        if capacity == 0 {
            send.wait_send(cx);
            return Poll::Pending;
        }

        Poll::Ready(Some(Ok(capacity)))
    }

    /// Current available stream send capacity
    pub fn capacity(&self, stream: &stream::Shared) -> WindowSize {
        stream.capacity(self.max_buffer_size)
    }

    pub fn open(&self) -> Result<StreamId, UserError> {
        self.next_stream_id.open()
    }

    pub fn reserve_local(&self) -> Result<StreamId, UserError> {
        self.next_stream_id.reserve_local()
    }

    pub fn ensure_next_stream_id(&self) -> Result<StreamId, UserError> {
        self.next_stream_id.ensure_next_stream_id()
    }

    pub fn ensure_not_idle(&self, id: StreamId) -> Result<(), Reason> {
        self.next_stream_id.ensure_not_idle(id)
    }

    pub fn may_have_created_stream(&self, id: StreamId) -> bool {
        self.next_stream_id.may_have_created_stream(id)
    }

    pub fn maybe_reset_next_stream_id(&self, id: StreamId) {
        self.next_stream_id.maybe_reset(id);
    }
}

// ===== stateless send operations

pub(super) fn poll_reset(
    cx: &Context,
    stream: &stream::Shared,
    mode: PollReset,
) -> Poll<Result<Reason, crate::Error>> {
    let state = stream.state();
    match state.state.ensure_reason(mode)? {
        Some(reason) => Poll::Ready(Ok(reason)),
        None => {
            drop(state);
            stream.send().wait_send(cx);
            Poll::Pending
        }
    }
}

pub(super) fn prepare_reserve_capacity(
    capacity: WindowSize,
    stream: &stream::Shared,
) -> Option<PreparedReserveCapacity> {
    let (buffered_send_data, requested_send_capacity) = {
        let send = stream.send();
        (send.buffered_send_data, send.requested_send_capacity)
    };
    let span = tracing::trace_span!(
        "reserve_capacity",
        ?stream.id,
        requested = capacity,
        effective = (capacity as usize) + buffered_send_data,
        curr = requested_send_capacity
    );
    let _e = span.enter();

    let capacity = (capacity as usize) + buffered_send_data;

    let mut shared = stream.send();

    match capacity.cmp(&(shared.requested_send_capacity as usize)) {
        Ordering::Equal => None,
        Ordering::Less => {
            shared.requested_send_capacity = capacity as WindowSize;

            let available = shared.send_flow.available().as_size();

            if available as usize > capacity {
                let diff = available - capacity as WindowSize;

                let _res = shared.send_flow.claim_capacity(diff);
                debug_assert!(_res.is_ok());

                Some(PreparedReserveCapacity::AssignConnection(diff))
            } else {
                None
            }
        }
        Ordering::Greater => {
            if stream.state().state.is_send_closed() {
                return None;
            }

            shared.requested_send_capacity =
                cmp::min(capacity, WindowSize::MAX as usize) as WindowSize;
            Some(PreparedReserveCapacity::TryAssign)
        }
    }
}

impl NextStreamId {
    const OVERFLOW: u64 = u64::MAX;

    pub(super) fn new(next_stream_id: StreamId) -> Self {
        Self(AtomicU64::new(Self::encode_ok(next_stream_id)))
    }

    pub fn open(&self) -> Result<StreamId, UserError> {
        self.take_next_stream_id()
    }

    pub fn reserve_local(&self) -> Result<StreamId, UserError> {
        self.take_next_stream_id()
    }

    pub fn ensure_not_idle(&self, id: StreamId) -> Result<(), Reason> {
        if let Ok(next) = self.load() {
            if id >= next {
                return Err(Reason::PROTOCOL_ERROR);
            }
        }
        // if next_stream_id is overflowed, that's ok.

        Ok(())
    }

    pub fn ensure_next_stream_id(&self) -> Result<StreamId, UserError> {
        self.load().map_err(|_| UserError::OverflowedStreamId)
    }

    pub fn may_have_created_stream(&self, id: StreamId) -> bool {
        if let Ok(next_id) = self.load() {
            // Peer::is_local_init should have been called beforehand
            debug_assert_eq!(id.is_server_initiated(), next_id.is_server_initiated(),);
            id < next_id
        } else {
            true
        }
    }

    pub(super) fn maybe_reset(&self, id: StreamId) {
        let _ = self.0.fetch_update(
            AtomicOrdering::Relaxed,
            AtomicOrdering::Relaxed,
            |current| {
                let next_id = match Self::decode(current) {
                    Ok(next_id) => next_id,
                    Err(_) => return None,
                };

                // Peer::is_local_init should have been called beforehand
                debug_assert_eq!(id.is_server_initiated(), next_id.is_server_initiated());

                if id >= next_id {
                    Some(Self::encode(id.next_id()))
                } else {
                    None
                }
            },
        );
    }

    fn take_next_stream_id(&self) -> Result<StreamId, UserError> {
        let stream_id = self
            .0
            .fetch_update(
                AtomicOrdering::Relaxed,
                AtomicOrdering::Relaxed,
                |current| {
                    let stream_id = match Self::decode(current) {
                        Ok(stream_id) => stream_id,
                        Err(_) => return None,
                    };

                    Some(Self::encode(stream_id.next_id()))
                },
            )
            .map_err(|_| UserError::OverflowedStreamId)?;

        Ok(Self::decode(stream_id).expect("successful reservation must start from a valid ID"))
    }

    fn load(&self) -> Result<StreamId, StreamIdOverflow> {
        Self::decode(self.0.load(AtomicOrdering::Relaxed))
    }

    fn encode(next_stream_id: Result<StreamId, StreamIdOverflow>) -> u64 {
        match next_stream_id {
            Ok(next_stream_id) => Self::encode_ok(next_stream_id),
            Err(_) => Self::OVERFLOW,
        }
    }

    fn encode_ok(next_stream_id: StreamId) -> u64 {
        u32::from(next_stream_id) as u64
    }

    fn decode(next_stream_id: u64) -> Result<StreamId, StreamIdOverflow> {
        if next_stream_id == Self::OVERFLOW {
            Err(StreamIdOverflow)
        } else {
            debug_assert!(next_stream_id <= u64::from(u32::from(StreamId::MAX)));
            Ok(StreamId::from(next_stream_id as u32))
        }
    }
}
