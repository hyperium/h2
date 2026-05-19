use super::counts;
use super::store::Resolve;
use super::streams::ConnWaker;
use super::*;

use crate::frame::Reason;

use bytes::buf::Take;
use std::{
    cmp::{self, Ordering},
    fmt, io, mem,
    task::{Context, Poll},
};

/// # Warning
///
/// Queued streams are ordered by stream ID, as we need to ensure that
/// lower-numbered streams are sent headers before higher-numbered ones.
/// This is because "idle" stream IDs – those which have been initiated but
/// have yet to receive frames – will be implicitly closed on receipt of a
/// frame on a higher stream ID. If these queues was not ordered by stream
/// IDs, some mechanism would be necessary to ensure that the lowest-numbered]
/// idle stream is opened first.
#[derive(Debug)]
pub(super) struct Prioritize {
    /// Queue of streams waiting for socket capacity to send a frame.
    pending_send: store::Queue<stream::NextSend>,

    /// Queue of streams waiting for window capacity to produce data.
    pending_capacity: store::Queue<stream::NextSendCapacity>,

    /// Streams waiting for capacity due to max concurrency
    ///
    /// The `SendRequest` handle is `Clone`. This enables initiating requests
    /// from many tasks. However, offering this capability while supporting
    /// backpressure at some level is tricky. If there are many `SendRequest`
    /// handles and a single stream becomes available, which handle gets
    /// assigned that stream? Maybe that handle is no longer ready to send a
    /// request.
    ///
    /// The strategy used is to allow each `SendRequest` handle one buffered
    /// request. A `SendRequest` handle is ready to send a request if it has no
    /// associated buffered requests. This is the same strategy as `mpsc` in the
    /// futures library.
    pending_open: store::Queue<stream::NextOpen>,

    /// Connection level flow control governing sent data
    flow: FlowControl,

    /// Stream ID of the last stream opened.
    last_opened_id: StreamId,

    /// What `DATA` frame is currently being sent in the codec.
    in_flight_data_frame: InFlightData,

    /// The maximum amount of bytes a stream should buffer.
    max_buffer_size: usize,
}

#[derive(Debug, Eq, PartialEq)]
enum InFlightData {
    /// There is no `DATA` frame in flight.
    Nothing,
    /// There is a `DATA` frame in flight belonging to the given stream.
    DataFrame(store::Key),
    /// There was a `DATA` frame, but the stream's queue was since cleared.
    Drop,
}

pub(crate) struct Prioritized<B> {
    // The buffer
    inner: Take<B>,

    end_of_stream: bool,

    // The stream that this is associated with
    stream: store::Key,
}

// ===== impl Prioritize =====

impl Prioritize {
    pub fn new(config: &Config) -> Prioritize {
        let mut flow = FlowControl::new();

        flow.inc_window(config.remote_init_window_sz)
            .expect("invalid initial window size");

        // TODO: proper error handling
        let _res = flow.assign_capacity(config.remote_init_window_sz);
        debug_assert!(_res.is_ok());

        tracing::trace!("Prioritize::new; flow={:?}", flow);

        Prioritize {
            pending_send: store::Queue::new(),
            pending_capacity: store::Queue::new(),
            pending_open: store::Queue::new(),
            flow,
            last_opened_id: StreamId::ZERO,
            in_flight_data_frame: InFlightData::Nothing,
            max_buffer_size: config.local_max_buffer_size,
        }
    }

    /// Queue a frame to be sent to the remote
    pub fn queue_frame<B>(
        &mut self,
        frame: Frame<B>,
        buffer: &mut Buffer<Frame<B>>,
        stream: &mut store::Ptr,
        conn_task: &ConnWaker,
    ) {
        let span = tracing::trace_span!("Prioritize::queue_frame", ?stream.id);
        let _e = span.enter();
        // Queue the frame in the buffer
        stream.send().pending_send.push_back(buffer, frame);
        self.schedule_send(stream, conn_task);
    }

    pub fn schedule_send(&mut self, stream: &mut store::Ptr, conn_task: &ConnWaker) {
        // If the stream is waiting to be opened, nothing more to do.
        if stream.is_send_ready() {
            tracing::trace!(?stream.id, "schedule_send");
            // Queue the stream
            self.pending_send.push(stream);

            // Notify the connection.
            conn_task.wake();
        }
    }

    pub fn queue_open(&mut self, stream: &mut store::Ptr) {
        stream.send().is_pending_open = true;
        self.pending_open.push(stream);
    }

    /// Request capacity to send data
    pub fn reserve_capacity(
        &mut self,
        capacity: WindowSize,
        stream: &mut store::Ptr,
        counts_shared: &counts::Shared,
        counts: &mut Counts,
    ) {
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

        // Actual capacity is `capacity` + the current amount of buffered data.
        // If it were less, then we could never send out the buffered data.
        let capacity = (capacity as usize) + buffered_send_data;

        enum Action {
            None,
            AssignConnection(WindowSize),
            TryAssign,
        }

        let action = {
            let mut shared = stream.send();

            match capacity.cmp(&(shared.requested_send_capacity as usize)) {
                Ordering::Equal => Action::None,
                Ordering::Less => {
                    // Update the target requested capacity
                    shared.requested_send_capacity = capacity as WindowSize;

                    // Currently available capacity assigned to the stream
                    let available = shared.send_flow.available().as_size();

                    // If the stream has more assigned capacity than requested, reclaim
                    // some for the connection
                    if available as usize > capacity {
                        let diff = available - capacity as WindowSize;

                        // TODO: proper error handling
                        let _res = shared.send_flow.claim_capacity(diff);
                        debug_assert!(_res.is_ok());

                        Action::AssignConnection(diff)
                    } else {
                        Action::None
                    }
                }
                Ordering::Greater => {
                    // If trying to *add* capacity, but the stream send side is closed,
                    // there's nothing to be done.
                    if stream.shared().state.is_send_closed() {
                        return;
                    }

                    // Update the target requested capacity
                    shared.requested_send_capacity =
                        cmp::min(capacity, WindowSize::MAX as usize) as WindowSize;

                    // Try to assign additional capacity to the stream. If none is
                    // currently available, the stream will be queued to receive some
                    // when more becomes available.
                    Action::TryAssign
                }
            }
        };

        match action {
            Action::None => {}
            Action::AssignConnection(diff) => {
                self.assign_connection_capacity(diff, stream, counts_shared, counts)
            }
            Action::TryAssign => self.try_assign_capacity(stream),
        }
    }

    pub fn recv_stream_window_update(
        &mut self,
        inc: WindowSize,
        stream: &mut store::Ptr,
    ) -> Result<(), Reason> {
        let (state, flow) = {
            let state = stream.shared().state.clone();
            let flow = stream.send().send_flow;
            (state, flow)
        };
        let span = tracing::trace_span!(
            "recv_stream_window_update",
            ?stream.id,
            ?state,
            inc,
            flow = ?flow
        );
        let _e = span.enter();

        {
            let is_send_closed = stream.shared().state.is_send_closed();
            let buffered_send_data = stream.send().buffered_send_data;
            if is_send_closed && buffered_send_data == 0 {
                // We can't send any data, so don't bother doing anything else.
                return Ok(());
            }
        }

        // Update the stream level flow control.
        stream.send().send_flow.inc_window(inc)?;

        // If the stream is waiting on additional capacity, then this will
        // assign it (if available on the connection) and notify the producer
        self.try_assign_capacity(stream);

        Ok(())
    }

    pub fn recv_connection_window_update(
        &mut self,
        inc: WindowSize,
        store: &mut Store,
        counts_shared: &counts::Shared,
        counts: &mut Counts,
    ) -> Result<(), Reason> {
        // Update the connection's window
        self.flow.inc_window(inc)?;

        self.assign_connection_capacity(inc, store, counts_shared, counts);
        Ok(())
    }

    /// Reclaim all capacity assigned to the stream and re-assign it to the
    /// connection
    pub fn reclaim_all_capacity(
        &mut self,
        stream: &mut store::Ptr,
        counts_shared: &counts::Shared,
        counts: &mut Counts,
    ) {
        let available = stream.send().send_flow.available().as_size();
        if available > 0 {
            {
                let mut shared = stream.send();
                // TODO: proper error handling
                let _res = shared.send_flow.claim_capacity(available);
                debug_assert!(_res.is_ok());
            }
            // Re-assign all capacity to the connection
            self.assign_connection_capacity(available, stream, counts_shared, counts);
        }
    }

    /// Reclaim just reserved capacity, not buffered capacity, and re-assign
    /// it to the connection
    pub fn reclaim_reserved_capacity(
        &mut self,
        stream: &mut store::Ptr,
        counts_shared: &counts::Shared,
        counts: &mut Counts,
    ) {
        // only reclaim reserved capacity that isn't already buffered
        let reserved = {
            let shared = stream.send();
            if shared.send_flow.available().as_size() as usize > shared.buffered_send_data {
                Some(
                    shared.send_flow.available().as_size()
                        - shared.buffered_send_data as WindowSize,
                )
            } else {
                None
            }
        };

        if let Some(reserved) = reserved {
            {
                let mut shared = stream.send();
                // Panic safety: due to how `reserved` is computed it can't be greater
                // than what's available.
                shared
                    .send_flow
                    .claim_capacity(reserved)
                    .expect("window size should be greater than reserved");
            }
            self.assign_connection_capacity(reserved, stream, counts_shared, counts);
        }
    }

    pub fn clear_pending_capacity(
        &mut self,
        counts_shared: &counts::Shared,
        store: &mut Store,
        counts: &mut Counts,
    ) {
        let span = tracing::trace_span!("clear_pending_capacity");
        let _e = span.enter();
        while let Some(stream) = self.pending_capacity.pop(store) {
            counts.transition(counts_shared, stream, |_, stream| {
                tracing::trace!(?stream.id, "clear_pending_capacity");
            })
        }
    }

    pub fn assign_connection_capacity<R>(
        &mut self,
        inc: WindowSize,
        store: &mut R,
        counts_shared: &counts::Shared,
        counts: &mut Counts,
    ) where
        R: Resolve,
    {
        let span = tracing::trace_span!("assign_connection_capacity", inc);
        let _e = span.enter();

        // TODO: proper error handling
        let _res = self.flow.assign_capacity(inc);
        debug_assert!(_res.is_ok());

        // Assign newly acquired capacity to streams pending capacity.
        while self.flow.available() > 0 {
            let stream = match self.pending_capacity.pop(store) {
                Some(stream) => stream,
                None => return,
            };

            // Streams pending capacity may have been reset before capacity
            // became available. In that case, the stream won't want any
            // capacity, and so we shouldn't "transition" on it, but just evict
            // it and continue the loop.
            if {
                let state = stream.shared();
                let send = stream.send();
                !(state.state.is_send_streaming() || send.buffered_send_data > 0)
            } {
                continue;
            }

            counts.transition(counts_shared, stream, |_, stream| {
                // Try to assign capacity to the stream. This will also re-queue the
                // stream if there isn't enough connection level capacity to fulfill
                // the capacity request.
                self.try_assign_capacity(stream);
            })
        }
    }

    /// Request capacity to send data
    pub(super) fn try_assign_capacity(&mut self, stream: &mut store::Ptr) {
        // Streams over the max concurrent count should not have capacity assign to avoid starving the connection
        // capacity for open streams
        if stream.send().is_pending_open {
            return;
        }

        let (total_requested, additional, buffered_send_data, window_size) = {
            let shared = stream.send();

            // Total requested should never go below actual assigned
            // (Note: the window size can go lower than assigned)
            debug_assert!(shared.send_flow.available() <= shared.requested_send_capacity as usize);

            // The amount of additional capacity that the stream requests.
            // Don't assign more than the window has available!
            (
                shared.requested_send_capacity,
                cmp::min(
                    shared.requested_send_capacity - shared.send_flow.available().as_size(),
                    // Can't assign more than what is available
                    shared.send_flow.window_size() - shared.send_flow.available().as_size(),
                ),
                shared.buffered_send_data,
                shared.send_flow.window_size(),
            )
        };
        let span = tracing::trace_span!("try_assign_capacity", ?stream.id);
        let _e = span.enter();
        tracing::trace!(
            requested = total_requested,
            additional,
            buffered = buffered_send_data,
            window = window_size,
            conn = %self.flow.available()
        );

        if additional == 0 {
            // Nothing more to do
            return;
        }

        {
            let state = stream.shared();
            let send = stream.send();
            // The stream may have been reset or closed since capacity was requested.
            if !state.state.is_send_streaming() && send.buffered_send_data == 0 {
                return;
            }
        }

        // The amount of currently available capacity on the connection
        let conn_available = self.flow.available().as_size();

        // First check if capacity is immediately available
        if conn_available > 0 {
            // The amount of capacity to assign to the stream
            // TODO: Should prioritization factor into this?
            let assign = cmp::min(conn_available, additional);

            tracing::trace!(capacity = assign, "assigning");

            // Assign the capacity to the stream
            stream.assign_capacity(assign, self.max_buffer_size);

            // Claim the capacity from the connection
            // TODO: proper error handling
            let _res = self.flow.claim_capacity(assign);
            debug_assert!(_res.is_ok());
        }

        let (should_queue_pending_capacity, has_buffered_send_data) = {
            let shared = stream.send();

            tracing::trace!(
                available = %shared.send_flow.available(),
                requested = shared.requested_send_capacity,
                buffered = shared.buffered_send_data,
                has_unavailable = %shared.send_flow.has_unavailable()
            );

            (
                shared.send_flow.available() < shared.requested_send_capacity as usize
                    && shared.send_flow.has_unavailable(),
                shared.buffered_send_data > 0,
            )
        };
        let should_queue_send = has_buffered_send_data && stream.is_send_ready();

        if should_queue_pending_capacity {
            // The stream requires additional capacity and the stream's
            // window has available capacity, but the connection window
            // does not.
            //
            // In this case, the stream needs to be queued up for when the
            // connection has more capacity.
            self.pending_capacity.push(stream);
        }

        // If data is buffered and the stream is send ready, then
        // schedule the stream for execution
        if should_queue_send {
            // TODO: This assertion isn't *exactly* correct. There can still be
            // buffered send data while the stream's pending send queue is
            // empty. This can happen when a large data frame is in the process
            // of being **partially** sent. Once the window has been sent, the
            // data frame will be returned to the prioritization layer to be
            // re-scheduled.
            //
            // That said, it would be nice to figure out how to make this
            // assertion correctly.
            //
            // debug_assert!(!stream.pending_send.is_empty());

            self.pending_send.push(stream);
        }
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
        // Ensure codec is ready
        ready!(dst.poll_ready(cx))?;

        // Reclaim any frame that has previously been written
        self.reclaim_frame(buffer, store, dst);

        // The max frame length
        let max_frame_len = dst.max_send_frame_size();

        tracing::trace!("poll_complete");

        loop {
            if let Some(mut stream) = self.pop_pending_open(store, counts_shared, counts) {
                self.pending_send.push_front(&mut stream);
                self.try_assign_capacity(&mut stream);
            }

            match self.pop_frame(buffer, store, max_frame_len, counts_shared, counts) {
                Some(frame) => {
                    tracing::trace!(?frame, "writing");

                    debug_assert_eq!(self.in_flight_data_frame, InFlightData::Nothing);
                    if let Frame::Data(ref frame) = frame {
                        self.in_flight_data_frame = InFlightData::DataFrame(frame.payload().stream);
                    }
                    dst.buffer(frame).expect("invalid frame");

                    // Ensure the codec is ready to try the loop again.
                    ready!(dst.poll_ready(cx))?;

                    // Because, always try to reclaim...
                    self.reclaim_frame(buffer, store, dst);
                }
                None => {
                    // Try to flush the codec.
                    ready!(dst.flush(cx))?;

                    // This might release a data frame...
                    if !self.reclaim_frame(buffer, store, dst) {
                        return Poll::Ready(Ok(()));
                    }

                    // No need to poll ready as poll_complete() does this for
                    // us...
                }
            }
        }
    }

    /// Tries to reclaim a pending data frame from the codec.
    ///
    /// Returns true if a frame was reclaimed.
    ///
    /// When a data frame is written to the codec, it may not be written in its
    /// entirety (large chunks are split up into potentially many data frames).
    /// In this case, the stream needs to be reprioritized.
    fn reclaim_frame<T, B>(
        &mut self,
        buffer: &mut Buffer<Frame<B>>,
        store: &mut Store,
        dst: &mut Codec<T, Prioritized<B>>,
    ) -> bool
    where
        B: Buf,
    {
        let span = tracing::trace_span!("try_reclaim_frame");
        let _e = span.enter();

        // First check if there are any data chunks to take back
        if let Some(frame) = dst.take_last_data_frame() {
            self.reclaim_frame_inner(buffer, store, frame)
        } else {
            false
        }
    }

    fn reclaim_frame_inner<B>(
        &mut self,
        buffer: &mut Buffer<Frame<B>>,
        store: &mut Store,
        frame: frame::Data<Prioritized<B>>,
    ) -> bool
    where
        B: Buf,
    {
        tracing::trace!(
            ?frame,
            sz = frame.payload().inner.get_ref().remaining(),
            "reclaimed"
        );

        let mut eos = false;
        let key = frame.payload().stream;

        match mem::replace(&mut self.in_flight_data_frame, InFlightData::Nothing) {
            InFlightData::Nothing => panic!("wasn't expecting a frame to reclaim"),
            InFlightData::Drop => {
                tracing::trace!("not reclaiming frame for cancelled stream");
                return false;
            }
            InFlightData::DataFrame(k) => {
                debug_assert_eq!(k, key);
            }
        }

        let mut frame = frame.map(|prioritized| {
            // TODO: Ensure fully written
            eos = prioritized.end_of_stream;
            prioritized.inner.into_inner()
        });

        if frame.payload().has_remaining() {
            let mut stream = store.resolve(key);

            if eos {
                frame.set_end_stream(true);
            }

            self.push_back_frame(frame.into(), buffer, &mut stream);

            return true;
        }

        false
    }

    /// Push the frame to the front of the stream's deque, scheduling the
    /// stream if needed.
    fn push_back_frame<B>(
        &mut self,
        frame: Frame<B>,
        buffer: &mut Buffer<Frame<B>>,
        stream: &mut store::Ptr,
    ) {
        let should_schedule = {
            let mut shared = stream.send();

            // Push the frame to the front of the stream's deque
            shared.pending_send.push_front(buffer, frame);

            let should_schedule = shared.send_flow.available() > 0;
            if should_schedule {
                debug_assert!(!shared.pending_send.is_empty());
            }

            should_schedule
        };

        // If needed, schedule the sender
        if should_schedule {
            self.pending_send.push(stream);
        }
    }

    pub fn clear_queue<B>(&mut self, buffer: &mut Buffer<Frame<B>>, stream: &mut store::Ptr) {
        let span = tracing::trace_span!("clear_queue", ?stream.id);
        let _e = span.enter();

        // TODO: make this more efficient?
        while let Some(frame) = stream.send().pending_send.pop_front(buffer) {
            tracing::trace!(?frame, "dropping");
        }
        while let Some(frame) = stream.send().pending_op_data.pop_front(buffer) {
            tracing::trace!(?frame, "dropping unapplied");
        }

        {
            let mut shared = stream.send();
            shared.buffered_send_data = 0;
            shared.requested_send_capacity = 0;
        }
        if let InFlightData::DataFrame(key) = self.in_flight_data_frame {
            if stream.key() == key {
                // This stream could get cleaned up now - don't allow the buffered frame to get reclaimed.
                self.in_flight_data_frame = InFlightData::Drop;
            }
        }
    }

    pub fn clear_pending_send(
        &mut self,
        counts_shared: &counts::Shared,
        store: &mut Store,
        counts: &mut Counts,
    ) {
        while let Some(stream) = self.pending_send.pop(store) {
            let is_pending_reset = stream.is_pending_reset_expiration();
            let scheduled_reset = {
                let shared = stream.shared();
                shared.state.get_scheduled_reset()
            };
            if let Some(reason) = scheduled_reset {
                stream.set_reset(reason, Initiator::Library);
            }
            counts.transition_after(counts_shared, stream, is_pending_reset);
        }
    }

    pub fn clear_pending_open(
        &mut self,
        counts_shared: &counts::Shared,
        store: &mut Store,
        counts: &mut Counts,
    ) {
        while let Some(stream) = self.pending_open.pop(store) {
            stream.send().is_pending_open = false;
            stream.notify_send();
            let is_pending_reset = stream.is_pending_reset_expiration();
            counts.transition_after(counts_shared, stream, is_pending_reset);
        }
    }

    fn pop_frame<B>(
        &mut self,
        buffer: &mut Buffer<Frame<B>>,
        store: &mut Store,
        max_len: usize,
        counts_shared: &counts::Shared,
        counts: &mut Counts,
    ) -> Option<Frame<Prioritized<B>>>
    where
        B: Buf,
    {
        let span = tracing::trace_span!("pop_frame");
        let _e = span.enter();

        loop {
            match self.pending_send.pop(store) {
                Some(mut stream) => {
                    let state = stream.shared().state.clone();
                    let span = tracing::trace_span!("popped", ?stream.id, ?state);
                    let _e = span.enter();

                    // It's possible that this stream, besides having data to send,
                    // is also queued to send a reset, and thus is already in the queue
                    // to wait for "some time" after a reset.
                    //
                    // To be safe, we just always ask the stream.
                    let is_pending_reset = stream.is_pending_reset_expiration();

                    tracing::trace!(is_pending_reset);

                    let next_frame = { stream.send().pending_send.pop_front(buffer) };
                    let frame = match next_frame {
                        Some(Frame::Data(mut frame)) => {
                            let scheduled_reset = { stream.shared().state.get_scheduled_reset() };
                            if let Some(reason) = scheduled_reset {
                                // If a reset is scheduled due to cancellation or
                                // an error, discard buffered DATA and let the `None`
                                // arm emit the RST_STREAM on the next iteration.
                                //
                                // NO_ERROR is excluded. Per RFC 9113 §8.1, a NO_ERROR
                                // stream reset may only be sent after a complete
                                // response, which requires sending all queued DATA.
                                if reason != Reason::NO_ERROR {
                                    stream.send().pending_send.push_front(buffer, frame.into());
                                    self.clear_queue(buffer, &mut stream);
                                    self.reclaim_all_capacity(&mut stream, counts_shared, counts);
                                    self.pending_send.push(&mut stream);
                                    continue;
                                }
                            }

                            // Get the amount of capacity remaining for stream's
                            // window.
                            let (stream_capacity, requested_send_capacity, buffered_send_data) = {
                                let shared = stream.send();
                                (
                                    shared.send_flow.available(),
                                    shared.requested_send_capacity,
                                    shared.buffered_send_data,
                                )
                            };
                            let sz = frame.payload().remaining();

                            tracing::trace!(
                                sz,
                                eos = frame.is_end_stream(),
                                window = %stream_capacity,
                                available = %stream_capacity,
                                requested = requested_send_capacity,
                                buffered = buffered_send_data,
                                "data frame"
                            );

                            // Zero length data frames always have capacity to
                            // be sent.
                            if sz > 0 && stream_capacity == 0 {
                                tracing::trace!("stream capacity is 0");

                                // Ensure that the stream is waiting for
                                // connection level capacity
                                //
                                // TODO: uncomment
                                // debug_assert!(stream.is_pending_send_capacity);

                                // The stream has no more capacity, this can
                                // happen if the remote reduced the stream
                                // window. In this case, we need to buffer the
                                // frame and wait for a window update...
                                stream.send().pending_send.push_front(buffer, frame.into());

                                continue;
                            }

                            // Only send up to the max frame length
                            let len = cmp::min(sz, max_len);

                            // Only send up to the stream's window capacity
                            let len =
                                cmp::min(len, stream_capacity.as_size() as usize) as WindowSize;

                            // There *must* be be enough connection level
                            // capacity at this point.
                            debug_assert!(len <= self.flow.window_size());

                            // Check if the stream level window the peer knows is available. In some
                            // scenarios, maybe the window we know is available but the window which
                            // peer knows is not.
                            if len > 0 && len > stream.send().send_flow.window_size() {
                                stream.send().pending_send.push_front(buffer, frame.into());
                                continue;
                            }

                            tracing::trace!(len, "sending data frame");

                            // Update the flow control
                            tracing::trace_span!("updating stream flow").in_scope(|| {
                                stream.send_data(len, self.max_buffer_size);

                                // Assign the capacity back to the connection that
                                // was just consumed from the stream in the previous
                                // line.
                                // TODO: proper error handling
                                let _res = self.flow.assign_capacity(len);
                                debug_assert!(_res.is_ok());
                            });

                            let (eos, len) = tracing::trace_span!("updating connection flow")
                                .in_scope(|| {
                                    // TODO: proper error handling
                                    let _res = self.flow.send_data(len);
                                    debug_assert!(_res.is_ok());

                                    // Wrap the frame's data payload to ensure that the
                                    // correct amount of data gets written.

                                    let eos = frame.is_end_stream();
                                    let len = len as usize;

                                    if frame.payload().remaining() > len {
                                        frame.set_end_stream(false);
                                    }
                                    (eos, len)
                                });

                            Frame::Data(frame.map(|buf| Prioritized {
                                inner: buf.take(len),
                                end_of_stream: eos,
                                stream: stream.key(),
                            }))
                        }
                        Some(Frame::PushPromise(pp)) => {
                            let mut pushed =
                                stream.store_mut().find_mut(&pp.promised_id()).unwrap();
                            let has_pending_send = {
                                let mut state = pushed.shared();
                                state.is_pending_push = false;
                                !pushed.send().pending_send.is_empty()
                            };
                            // Transition stream from pending_push to pending_open
                            // if possible
                            if has_pending_send {
                                if counts.can_inc_num_send_streams(counts_shared) {
                                    counts.inc_num_send_streams(counts_shared, &mut pushed);
                                    self.pending_send.push(&mut pushed);
                                } else {
                                    self.queue_open(&mut pushed);
                                }
                            }
                            Frame::PushPromise(pp)
                        }
                        Some(frame) => frame.map(|_| {
                            unreachable!(
                                "Frame::map closure will only be called \
                                 on DATA frames."
                            )
                        }),
                        None => {
                            let scheduled_reset = {
                                let shared = stream.shared();
                                shared.state.get_scheduled_reset()
                            };
                            if let Some(reason) = scheduled_reset {
                                stream.set_reset(reason, Initiator::Library);

                                let frame = frame::Reset::new(stream.id, reason);
                                Frame::Reset(frame)
                            } else {
                                // If the stream receives a RESET from the peer, it may have
                                // had data buffered to be sent, but all the frames are cleared
                                // in clear_queue(). Instead of doing O(N) traversal through queue
                                // to remove, lets just ignore the stream here.
                                tracing::trace!("removing dangling stream from pending_send");
                                // Since this should only happen as a consequence of `clear_queue`,
                                // we must be in a closed state of some kind.
                                let is_closed = stream.shared().state.is_closed();
                                debug_assert!(is_closed);
                                counts.transition_after(counts_shared, stream, is_pending_reset);
                                continue;
                            }
                        }
                    };

                    tracing::trace!("pop_frame; frame={:?}", frame);

                    if cfg!(debug_assertions) && stream.shared().state.is_idle() {
                        debug_assert!(stream.id > self.last_opened_id);
                        self.last_opened_id = stream.id;
                    }

                    let should_requeue = {
                        let has_pending_send = !stream.send().pending_send.is_empty();
                        let is_scheduled_reset = stream.shared().state.is_scheduled_reset();
                        has_pending_send || is_scheduled_reset
                    };
                    if should_requeue {
                        // TODO: Only requeue the sender IF it is ready to send
                        // the next frame. i.e. don't requeue it if the next
                        // frame is a data frame and the stream does not have
                        // any more capacity.
                        self.pending_send.push(&mut stream);
                    }

                    counts.transition_after(counts_shared, stream, is_pending_reset);

                    return Some(frame);
                }
                None => return None,
            }
        }
    }

    fn pop_pending_open<'s>(
        &mut self,
        store: &'s mut Store,
        counts_shared: &counts::Shared,
        counts: &mut Counts,
    ) -> Option<store::Ptr<'s>> {
        tracing::trace!("schedule_pending_open");
        // check for any pending open streams
        if counts.can_inc_num_send_streams(counts_shared) {
            if let Some(mut stream) = self.pending_open.pop(store) {
                tracing::trace!("schedule_pending_open; stream={:?}", stream.id);

                counts.inc_num_send_streams(counts_shared, &mut stream);
                stream.send().is_pending_open = false;
                stream.notify_send();
                return Some(stream);
            }
        }

        None
    }
}

// ===== impl Prioritized =====

impl<B> Buf for Prioritized<B>
where
    B: Buf,
{
    fn remaining(&self) -> usize {
        self.inner.remaining()
    }

    fn chunk(&self) -> &[u8] {
        self.inner.chunk()
    }

    fn chunks_vectored<'a>(&'a self, dst: &mut [std::io::IoSlice<'a>]) -> usize {
        self.inner.chunks_vectored(dst)
    }

    fn advance(&mut self, cnt: usize) {
        self.inner.advance(cnt)
    }
}

impl<B: Buf> fmt::Debug for Prioritized<B> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("Prioritized")
            .field("remaining", &self.inner.get_ref().remaining())
            .field("end_of_stream", &self.end_of_stream)
            .field("stream", &self.stream)
            .finish()
    }
}
