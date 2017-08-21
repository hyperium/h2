use super::*;

use bytes::buf::Take;

use std::{fmt, cmp};

#[derive(Debug)]
pub(super) struct Prioritize<B> {
    /// Queue of streams waiting for socket capacity to send a frame
    pending_send: store::Queue<B, stream::Next>,

    /// Queue of streams waiting for window capacity to produce data.
    pending_capacity: store::Queue<B, stream::NextSendCapacity>,

    /// Connection level flow control governing sent data
    flow: FlowControl,

    /// Holds frames that are waiting to be written to the socket
    buffer: Buffer<B>,

    /// Holds the connection task. This signals the connection that there is
    /// data to flush.
    conn_task: Option<task::Task>,
}

pub(crate) struct Prioritized<B> {
    // The buffer
    inner: Take<B>,

    end_of_stream: bool,

    // The stream that this is associated with
    stream: store::Key,
}

// ===== impl Prioritize =====

impl<B> Prioritize<B>
    where B: Buf,
{
    pub fn new(config: &Config) -> Prioritize<B> {
        let mut flow = FlowControl::new();

        flow.inc_window(config.init_local_window_sz);
        flow.assign_capacity(config.init_local_window_sz);

        Prioritize {
            pending_send: store::Queue::new(),
            pending_capacity: store::Queue::new(),
            flow: flow,
            buffer: Buffer::new(),
            conn_task: None,
        }
    }

    /// Queue a frame to be sent to the remote
    pub fn queue_frame(&mut self,
                       frame: Frame<B>,
                       stream: &mut store::Ptr<B>)
    {
        // Queue the frame in the buffer
        stream.pending_send.push_back(&mut self.buffer, frame);

        // Queue the stream
        self.pending_send.push(stream);

        // Notify the connection.
        if let Some(task) = self.conn_task.take() {
            task.notify();
        }
    }

    /// Send a data frame
    pub fn send_data(&mut self,
                     frame: frame::Data<B>,
                     stream: &mut store::Ptr<B>)
        -> Result<(), ConnectionError>
    {
        let sz = frame.payload().remaining();

        if sz > MAX_WINDOW_SIZE as usize {
            // TODO: handle overflow
            unimplemented!();
        }

        let sz = sz as WindowSize;

        if !stream.state.is_send_streaming() {
            if stream.state.is_closed() {
                return Err(InactiveStreamId.into());
            } else {
                return Err(UnexpectedFrameType.into());
            }
        }

        // Update the buffered data counter
        stream.buffered_send_data += sz;

        // Implicitly request more send capacity if not enough has been
        // requested yet.
        if stream.requested_send_capacity < stream.buffered_send_data {
            // Update the target requested capacity
            stream.requested_send_capacity = stream.buffered_send_data;

            self.try_assign_capacity(stream);
        }

        if frame.is_end_stream() {
            try!(stream.state.send_close());
        }

        if stream.send_flow.available() > stream.buffered_send_data {
            // The stream currently has capacity to send the data frame, so
            // queue it up and notify the connection task.
            self.queue_frame(frame.into(), stream);
        } else {
            // The stream has no capacity to send the frame now, save it but
            // don't notify the conneciton task. Once additional capacity
            // becomes available, the frame will be flushed.
            stream.pending_send.push_back(&mut self.buffer, frame.into());
        }

        Ok(())
    }

    /// Request capacity to send data
    pub fn reserve_capacity(&mut self, capacity: WindowSize, stream: &mut store::Ptr<B>)
        -> Result<(), ConnectionError>
    {
        // Actual capacity is `capacity` + the current amount of buffered data.
        // It it were less, then we could never send out the buffered data.
        let capacity = capacity + stream.buffered_send_data;

        if capacity == stream.requested_send_capacity {
            // Nothing to do
            return Ok(());
        } else if capacity < stream.requested_send_capacity {
            // TODO: release capacity
            unimplemented!();
        } else {
            // Update the target requested capacity
            stream.requested_send_capacity = capacity;

            // Try to assign additional capacity to the stream. If none is
            // currently available, the stream will be queued to receive some
            // when more becomes available.
            self.try_assign_capacity(stream);

            Ok(())
        }
    }

    pub fn recv_stream_window_update(&mut self,
                                     inc: WindowSize,
                                     stream: &mut store::Ptr<B>)
        -> Result<(), ConnectionError>
    {
        if !stream.state.is_send_streaming() {
            return Ok(());
        }

        // Update the stream level flow control.
        stream.send_flow.inc_window(inc)?;

        // If the stream is waiting on additional capacity, then this will
        // assign it (if available on the connection) and notify the producer
        self.try_assign_capacity(stream);

        Ok(())
    }

    pub fn recv_connection_window_update(&mut self,
                                         inc: WindowSize,
                                         store: &mut Store<B>)
        -> Result<(), ConnectionError>
    {
        // Update the connection's window
        self.flow.inc_window(inc)?;

        // Assign newly acquired capacity to streams pending capacity.
        while self.flow.available() > 0 {
            let mut stream = match self.pending_capacity.pop(store) {
                Some(stream) => stream,
                None => return Ok(()),
            };

            // Try to assign capacity to the stream. This will also re-queue the
            // stream if there isn't enough connection level capacity to fulfill
            // the capacity request.
            self.try_assign_capacity(&mut stream);
        }

        Ok(())
    }

    /// Request capacity to send data
    fn try_assign_capacity(&mut self, stream: &mut store::Ptr<B>) {
        let total_requested = stream.requested_send_capacity;

        // Total requested should never go below actual assigned
        // (Note: the window size can go lower than assigned)
        debug_assert!(total_requested >= stream.send_flow.available());

        // The amount of additional capacity that the stream requests.
        // Don't assign more than the window has available!
        let mut additional = cmp::min(
            total_requested - stream.send_flow.available(),
            stream.send_flow.window_size());

        trace!("try_assign_capacity; requested={}; additional={}; conn={}",
               total_requested, additional, self.flow.available());

        if additional == 0 {
            // Nothing more to do
            return;
        }

        // The amount of currently available capacity on the connection
        let conn_available = self.flow.available();

        // First check if capacity is immediately available
        if conn_available > 0 {
            // There should be no streams pending capacity
            debug_assert!(self.pending_capacity.is_empty());

            // The amount of capacity to assign to the stream
            // TODO: Should prioritization factor into this?
            let assign = cmp::min(conn_available, additional);

            // Assign the capacity to the stream
            stream.assign_capacity(assign);

            // Claim the capacity from the connection
            self.flow.claim_capacity(assign);
        }

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
            self.pending_send.push(stream);
        }
    }


    pub fn poll_complete<T>(&mut self,
                            store: &mut Store<B>,
                            dst: &mut Codec<T, Prioritized<B>>)
        -> Poll<(), ConnectionError>
        where T: AsyncWrite,
    {
        // Ensure codec is ready
        try_ready!(dst.poll_ready());

        // Reclaim any frame that has previously been written
        self.reclaim_frame(store, dst);

        // The max frame length
        let max_frame_len = dst.max_send_frame_size();

        trace!("poll_complete");

        loop {
            match self.pop_frame(store, max_frame_len) {
                Some(frame) => {
                    trace!("writing frame={:?}", frame);

                    let res = dst.start_send(frame)?;

                    // We already verified that `dst` is ready to accept the
                    // write
                    assert!(res.is_ready());

                    // Ensure the codec is ready to try the loop again.
                    try_ready!(dst.poll_ready());

                    // Because, always try to reclaim...
                    self.reclaim_frame(store, dst);

                }
                None => {
                    // Try to flush the codec.
                    try_ready!(dst.poll_complete());

                    // This might release a data frame...
                    if !self.reclaim_frame(store, dst) {
                        // Nothing else to do, track the task
                        self.conn_task = Some(task::current());

                        return Ok(().into());
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
    fn reclaim_frame<T>(&mut self,
                        store: &mut Store<B>,
                        dst: &mut Codec<T, Prioritized<B>>) -> bool
    {
        trace!("try reclaim frame");

        // First check if there are any data chunks to take back
        if let Some(frame) = dst.take_last_data_frame() {
            trace!("  -> reclaimed; frame={:?}", frame);

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
                    frame.set_end_stream();
                }

                self.push_back_frame(frame.into(), &mut stream);

                return true;
            }
        }

        false
    }

    /// Push the frame to the front of the stream's deque, scheduling the
    /// steream if needed.
    fn push_back_frame(&mut self, frame: Frame<B>, stream: &mut store::Ptr<B>) {
        // Push the frame to the front of the stream's deque
        stream.pending_send.push_front(&mut self.buffer, frame);

        // If needed, schedule the sender
        self.pending_send.push(stream);
    }

    // =========== OLD JUNK ===========

    fn pop_frame(&mut self, store: &mut Store<B>, max_len: usize)
        -> Option<Frame<Prioritized<B>>>
    {
        loop {
            trace!("pop frame");
            match self.pending_send.pop(store) {
                Some(mut stream) => {
                    let frame = match stream.pending_send.pop_front(&mut self.buffer).unwrap() {
                        Frame::Data(mut frame) => {
                            trace!(" --> data frame");

                            // Get the amount of capacity remaining for stream's
                            // window.
                            //
                            // TODO: Is this the right thing to check?
                            let stream_capacity = stream.send_flow.window_size();

                            if stream_capacity == 0 {
                                trace!(" --> stream capacity is 0, return");
                                // The stream has no more capacity, this can
                                // happen if the remote reduced the stream
                                // window. In this case, we need to buffer the
                                // frame and wait for a window update...
                                stream.pending_send.push_front(&mut self.buffer, frame.into());
                                continue;
                            }

                            // Only send up to the max frame length
                            let len = cmp::min(
                                frame.payload().remaining(),
                                max_len);

                            // Only send up to the stream's window capacity
                            let len = cmp::min(len, stream_capacity as usize);

                            // There *must* be be enough connection level
                            // capacity at this point.
                            debug_assert!(len <= self.flow.window_size() as usize);

                            // Update the flow control
                            trace!(" -- updating stream flow --");
                            stream.send_flow.send_data(len as WindowSize);

                            // Assign the capacity back to the connection that
                            // was just consumed from the stream in the previous
                            // line.
                            self.flow.assign_capacity(len as WindowSize);

                            trace!(" -- updating connection flow --");
                            self.flow.send_data(len as WindowSize);

                            // Wrap the frame's data payload to ensure that the
                            // correct amount of data gets written.

                            let eos = frame.is_end_stream();

                            if frame.payload().remaining() > len {
                                frame.unset_end_stream();
                            }

                            Frame::Data(frame.map(|buf| {
                                Prioritized {
                                    inner: buf.take(len),
                                    end_of_stream: eos,
                                    stream: stream.key(),
                                }
                            }))
                        }
                        frame => frame.map(|_| unreachable!()),
                    };

                    if !stream.pending_send.is_empty() {
                        self.pending_send.push(&mut stream);
                    }

                    return Some(frame);
                }
                None => return None,
            }
        }
    }
}

// ===== impl Prioritized =====

impl<B> Buf for Prioritized<B>
    where B: Buf,
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
