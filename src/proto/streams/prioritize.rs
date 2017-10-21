use super::*;
use super::store::Resolve;

use frame::Reason;

use codec::UserError;
use codec::UserError::*;

use bytes::buf::Take;

use std::{cmp, fmt};
use std::io;

#[derive(Debug)]
pub(super) struct Prioritize {
    /// Queue of streams waiting for socket capacity to send a frame
    pending_send: store::Queue<stream::NextSend>,

    /// Queue of streams waiting for window capacity to produce data.
    pending_capacity: store::Queue<stream::NextSendCapacity>,

    /// Streams waiting for capacity due to max concurrency
    pending_open: store::Queue<stream::NextOpen>,

    /// Connection level flow control governing sent data
    flow: FlowControl,
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
            .ok()
            .expect("invalid initial window size");

        flow.assign_capacity(config.remote_init_window_sz);

        trace!("Prioritize::new; flow={:?}", flow);

        Prioritize {
            pending_send: store::Queue::new(),
            pending_capacity: store::Queue::new(),
            pending_open: store::Queue::new(),
            flow: flow,
        }
    }

    /// Queue a frame to be sent to the remote
    pub fn queue_frame<B>(
        &mut self,
        frame: Frame<B>,
        buffer: &mut Buffer<Frame<B>>,
        stream: &mut store::Ptr,
        task: &mut Option<Task>,
    ) {
        // Queue the frame in the buffer
        stream.pending_send.push_back(buffer, frame);
        self.schedule_send(stream, task);
    }

    pub fn schedule_send(&mut self, stream: &mut store::Ptr, task: &mut Option<Task>) {
        // If the stream is waiting to be opened, nothing more to do.
        if !stream.is_pending_open {
            // Queue the stream
            self.pending_send.push(stream);

            // Notify the connection.
            if let Some(task) = task.take() {
                task.notify();
            }
        }
    }

    pub fn queue_open(&mut self, stream: &mut store::Ptr) {
        self.pending_open.push(stream);
    }

    /// Send a data frame
    pub fn send_data<B>(
        &mut self,
        frame: frame::Data<B>,
        buffer: &mut Buffer<Frame<B>>,
        stream: &mut store::Ptr,
        task: &mut Option<Task>,
    ) -> Result<(), UserError>
    where
        B: Buf,
    {
        let sz = frame.payload().remaining();

        if sz > MAX_WINDOW_SIZE as usize {
            return Err(UserError::PayloadTooBig);
        }

        let sz = sz as WindowSize;

        if !stream.state.is_send_streaming() {
            if stream.state.is_closed() {
                return Err(InactiveStreamId);
            } else {
                return Err(UnexpectedFrameType);
            }
        }

        // Update the buffered data counter
        stream.buffered_send_data += sz;

        trace!(
            "send_data; sz={}; buffered={}; requested={}",
            sz,
            stream.buffered_send_data,
            stream.requested_send_capacity
        );

        // Implicitly request more send capacity if not enough has been
        // requested yet.
        if stream.requested_send_capacity < stream.buffered_send_data {
            // Update the target requested capacity
            stream.requested_send_capacity = stream.buffered_send_data;

            self.try_assign_capacity(stream);
        }

        if frame.is_end_stream() {
            stream.state.send_close();
            self.reserve_capacity(0, stream);
        }

        trace!(
            "send_data (2); available={}; buffered={}",
            stream.send_flow.available(),
            stream.buffered_send_data
        );

        if stream.send_flow.available() >= stream.buffered_send_data {
            // The stream currently has capacity to send the data frame, so
            // queue it up and notify the connection task.
            self.queue_frame(frame.into(), buffer, stream, task);
        } else {
            // The stream has no capacity to send the frame now, save it but
            // don't notify the conneciton task. Once additional capacity
            // becomes available, the frame will be flushed.
            stream
                .pending_send
                .push_back(buffer, frame.into());
        }

        Ok(())
    }

    /// Request capacity to send data
    pub fn reserve_capacity(&mut self, capacity: WindowSize, stream: &mut store::Ptr) {
        trace!(
            "reserve_capacity; stream={:?}; requested={:?}; effective={:?}; curr={:?}",
            stream.id,
            capacity,
            capacity + stream.buffered_send_data,
            stream.requested_send_capacity
        );

        // Actual capacity is `capacity` + the current amount of buffered data.
        // It it were less, then we could never send out the buffered data.
        let capacity = capacity + stream.buffered_send_data;

        if capacity == stream.requested_send_capacity {
            // Nothing to do
        } else if capacity < stream.requested_send_capacity {
            // Update the target requested capacity
            stream.requested_send_capacity = capacity;

            // Currently available capacity assigned to the stream
            let available = stream.send_flow.available().as_size();

            // If the stream has more assigned capacity than requested, reclaim
            // some for the connection
            if available > capacity {
                let diff = available - capacity;

                stream.send_flow.claim_capacity(diff);

                self.assign_connection_capacity(diff, stream);
            }
        } else {
            // Update the target requested capacity
            stream.requested_send_capacity = capacity;

            // Try to assign additional capacity to the stream. If none is
            // currently available, the stream will be queued to receive some
            // when more becomes available.
            self.try_assign_capacity(stream);
        }
    }

    pub fn recv_stream_window_update(
        &mut self,
        inc: WindowSize,
        stream: &mut store::Ptr,
    ) -> Result<(), Reason> {
        trace!(
            "recv_stream_window_update; stream={:?}; state={:?}; inc={}; flow={:?}",
            stream.id,
            stream.state,
            inc,
            stream.send_flow
        );

        // Update the stream level flow control.
        stream.send_flow.inc_window(inc)?;

        // If the stream is waiting on additional capacity, then this will
        // assign it (if available on the connection) and notify the producer
        self.try_assign_capacity(stream);

        Ok(())
    }

    pub fn recv_connection_window_update(
        &mut self,
        inc: WindowSize,
        store: &mut Store,
    ) -> Result<(), Reason> {
        // Update the connection's window
        self.flow.inc_window(inc)?;

        self.assign_connection_capacity(inc, store);
        Ok(())
    }

    pub fn assign_connection_capacity<R>(&mut self, inc: WindowSize, store: &mut R)
    where
        R: Resolve,
    {
        trace!("assign_connection_capacity; inc={}", inc);

        self.flow.assign_capacity(inc);

        // Assign newly acquired capacity to streams pending capacity.
        while self.flow.available() > 0 {
            let mut stream = match self.pending_capacity.pop(store) {
                Some(stream) => stream,
                None => return,
            };

            // Try to assign capacity to the stream. This will also re-queue the
            // stream if there isn't enough connection level capacity to fulfill
            // the capacity request.
            self.try_assign_capacity(&mut stream);
        }
    }

    /// Request capacity to send data
    fn try_assign_capacity(&mut self, stream: &mut store::Ptr) {
        let total_requested = stream.requested_send_capacity;

        // Total requested should never go below actual assigned
        // (Note: the window size can go lower than assigned)
        debug_assert!(total_requested >= stream.send_flow.available());

        // The amount of additional capacity that the stream requests.
        // Don't assign more than the window has available!
        let additional = cmp::min(
            total_requested - stream.send_flow.available().as_size(),
            // Can't assign more than what is available
            stream.send_flow.window_size() - stream.send_flow.available().as_size(),
        );

        trace!(
            "try_assign_capacity; requested={}; additional={}; buffered={}; window={}; conn={}",
            total_requested,
            additional,
            stream.buffered_send_data,
            stream.send_flow.window_size(),
            self.flow.available()
        );

        if additional == 0 {
            // Nothing more to do
            return;
        }

        // If the stream has requested capacity, then it must be in the
        // streaming state (more data could be sent) or there is buffered data
        // waiting to be sent.
        debug_assert!(
            stream.state.is_send_streaming() || stream.buffered_send_data > 0,
            "state={:?}",
            stream.state
        );

        // The amount of currently available capacity on the connection
        let conn_available = self.flow.available().as_size();

        // First check if capacity is immediately available
        if conn_available > 0 {
            // There should be no streams pending capacity
            debug_assert!(self.pending_capacity.is_empty());

            // The amount of capacity to assign to the stream
            // TODO: Should prioritization factor into this?
            let assign = cmp::min(conn_available, additional);

            trace!("  assigning; num={}", assign);

            // Assign the capacity to the stream
            stream.assign_capacity(assign);

            // Claim the capacity from the connection
            self.flow.claim_capacity(assign);
        }

        trace!(
            "try_assign_capacity; available={}; requested={}; buffered={}; has_unavailable={:?}",
            stream.send_flow.available(),
            stream.requested_send_capacity,
            stream.buffered_send_data,
            stream.send_flow.has_unavailable()
        );

        if stream.send_flow.available() < stream.requested_send_capacity {
            if stream.send_flow.has_unavailable() {
                // The stream requires additional capacity and the stream's
                // window has availablel capacity, but the connection window
                // does not.
                //
                // In this case, the stream needs to be queued up for when the
                // connection has more capacity.
                self.pending_capacity.push(stream);
            }
        }

        // If data is buffered, then schedule the stream for execution
        if stream.buffered_send_data > 0 {
            debug_assert!(stream.send_flow.available() > 0);

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
        buffer: &mut Buffer<Frame<B>>,
        store: &mut Store,
        counts: &mut Counts,
        dst: &mut Codec<T, Prioritized<B>>,
    ) -> Poll<(), io::Error>
    where
        T: AsyncWrite,
        B: Buf,
    {
        // Ensure codec is ready
        try_ready!(dst.poll_ready());

        // Reclaim any frame that has previously been written
        self.reclaim_frame(buffer, store, dst);

        // The max frame length
        let max_frame_len = dst.max_send_frame_size();

        trace!("poll_complete");

        loop {
            self.schedule_pending_open(store, counts);

            match self.pop_frame(buffer, store, max_frame_len, counts) {
                Some(frame) => {
                    trace!("writing frame={:?}", frame);

                    dst.buffer(frame).ok().expect("invalid frame");

                    // Ensure the codec is ready to try the loop again.
                    try_ready!(dst.poll_ready());

                    // Because, always try to reclaim...
                    self.reclaim_frame(buffer, store, dst);
                },
                None => {
                    // Try to flush the codec.
                    try_ready!(dst.flush());

                    // This might release a data frame...
                    if !self.reclaim_frame(buffer, store, dst) {
                        return Ok(().into());
                    }

                    // No need to poll ready as poll_complete() does this for
                    // us...
                },
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
        trace!("try reclaim frame");

        // First check if there are any data chunks to take back
        if let Some(frame) = dst.take_last_data_frame() {
            trace!(
                "  -> reclaimed; frame={:?}; sz={}",
                frame,
                frame.payload().remaining()
            );

            let mut eos = false;
            let key = frame.payload().stream;

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
        }

        false
    }

    /// Push the frame to the front of the stream's deque, scheduling the
    /// steream if needed.
    fn push_back_frame<B>(&mut self,
                          frame: Frame<B>,
                          buffer: &mut Buffer<Frame<B>>,
                          stream: &mut store::Ptr)
    {
        // Push the frame to the front of the stream's deque
        stream.pending_send.push_front(buffer, frame);

        // If needed, schedule the sender
        if stream.send_flow.available() > 0 {
            debug_assert!(!stream.pending_send.is_empty());
            self.pending_send.push(stream);
        }
    }

    pub fn clear_queue<B>(&mut self, buffer: &mut Buffer<Frame<B>>, stream: &mut store::Ptr) {
        trace!("clear_queue; stream-id={:?}", stream.id);

        // TODO: make this more efficient?
        while let Some(frame) = stream.pending_send.pop_front(buffer) {
            trace!("dropping; frame={:?}", frame);
        }
    }

    fn pop_frame<B>(
        &mut self,
        buffer: &mut Buffer<Frame<B>>,
        store: &mut Store,
        max_len: usize,
        counts: &mut Counts,
    ) -> Option<Frame<Prioritized<B>>>
    where
        B: Buf,
    {
        trace!("pop_frame");

        loop {
            match self.pending_send.pop(store) {
                Some(mut stream) => {
                    trace!("pop_frame; stream={:?}", stream.id);

                    let is_counted = stream.is_counted();

                    let frame = match stream.pending_send.pop_front(buffer) {
                        Some(Frame::Data(mut frame)) => {
                            // Get the amount of capacity remaining for stream's
                            // window.
                            let stream_capacity = stream.send_flow.available();
                            let sz = frame.payload().remaining();

                            trace!(
                                " --> data frame; stream={:?}; sz={}; eos={:?}; window={}; \
                                 available={}; requested={}",
                                frame.stream_id(),
                                sz,
                                frame.is_end_stream(),
                                stream_capacity,
                                stream.send_flow.available(),
                                stream.requested_send_capacity
                            );

                            // Zero length data frames always have capacity to
                            // be sent.
                            if sz > 0 && stream_capacity == 0 {
                                trace!(
                                    " --> stream capacity is 0; requested={}",
                                    stream.requested_send_capacity
                                );

                                // Ensure that the stream is waiting for
                                // connection level capacity
                                //
                                // TODO: uncomment
                                // debug_assert!(stream.is_pending_send_capacity);

                                // The stream has no more capacity, this can
                                // happen if the remote reduced the stream
                                // window. In this case, we need to buffer the
                                // frame and wait for a window update...
                                stream
                                    .pending_send
                                    .push_front(buffer, frame.into());

                                continue;
                            }

                            // Only send up to the max frame length
                            let len = cmp::min(sz, max_len);

                            // Only send up to the stream's window capacity
                            let len = cmp::min(len, stream_capacity.as_size() as usize) as WindowSize;

                            // There *must* be be enough connection level
                            // capacity at this point.
                            debug_assert!(len <= self.flow.window_size());

                            trace!(" --> sending data frame; len={}", len);

                            // Update the flow control
                            trace!(" -- updating stream flow --");
                            stream.send_flow.send_data(len);

                            // Decrement the stream's buffered data counter
                            debug_assert!(stream.buffered_send_data >= len);
                            stream.buffered_send_data -= len;
                            stream.requested_send_capacity -= len;

                            // Assign the capacity back to the connection that
                            // was just consumed from the stream in the previous
                            // line.
                            self.flow.assign_capacity(len);

                            trace!(" -- updating connection flow --");
                            self.flow.send_data(len);

                            // Wrap the frame's data payload to ensure that the
                            // correct amount of data gets written.

                            let eos = frame.is_end_stream();
                            let len = len as usize;

                            if frame.payload().remaining() > len {
                                frame.set_end_stream(false);
                            }

                            Frame::Data(frame.map(|buf| {
                                Prioritized {
                                    inner: buf.take(len),
                                    end_of_stream: eos,
                                    stream: stream.key(),
                                }
                            }))
                        },
                        Some(frame) => frame.map(|_|
                            unreachable!(
                                "Frame::map closure will only be called \
                                 on DATA frames."
                             )
                        ),
                        None => {
                            assert!(stream.state.is_canceled());
                            stream.state.set_reset(Reason::CANCEL);

                            let frame = frame::Reset::new(stream.id, Reason::CANCEL);
                            Frame::Reset(frame)
                        }
                    };

                    trace!("pop_frame; frame={:?}", frame);

                    if !stream.pending_send.is_empty() || stream.state.is_canceled() {
                        // TODO: Only requeue the sender IF it is ready to send
                        // the next frame. i.e. don't requeue it if the next
                        // frame is a data frame and the stream does not have
                        // any more capacity.
                        self.pending_send.push(&mut stream);
                    }

                    counts.transition_after(stream, is_counted);

                    return Some(frame);
                },
                None => return None,
            }
        }
    }

    fn schedule_pending_open(&mut self, store: &mut Store, counts: &mut Counts) {
        trace!("schedule_pending_open");
        // check for any pending open streams
        while counts.can_inc_num_send_streams() {
            if let Some(mut stream) = self.pending_open.pop(store) {
                trace!("schedule_pending_open; stream={:?}", stream.id);
                counts.inc_num_send_streams();
                self.pending_send.push(&mut stream);
                if let Some(task) = stream.open_task.take() {
                    task.notify();
                }
            } else {
                return;
            }
        }
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

    fn bytes(&self) -> &[u8] {
        self.inner.bytes()
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
