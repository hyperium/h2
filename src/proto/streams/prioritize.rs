use super::*;

use bytes::buf::Take;

use std::{fmt, cmp};

#[derive(Debug)]
pub(super) struct Prioritize<B> {
    /// Streams that have pending frames
    pending_send: store::List<B>,

    /// Streams that are waiting for connection level flow control capacity
    pending_capacity: store::List<B>,

    /// Connection level flow control governing sent data
    flow_control: FlowControl,

    /// Total amount of buffered data in data frames
    buffered_data: usize,

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
        Prioritize {
            pending_send: store::List::new(),
            pending_capacity: store::List::new(),
            flow_control: FlowControl::with_window_size(config.init_local_window_sz),
            buffered_data: 0,
            buffer: Buffer::new(),
            conn_task: None,
        }
    }

    pub fn available_window(&self) -> WindowSize {
        unimplemented!();
        /*
        let win = self.flow_control.effective_window_size();

        if self.buffered_data >= win as usize {
            0
        } else {
            win - self.buffered_data as WindowSize
        }
        */
    }

    pub fn recv_window_update(&mut self, frame: frame::WindowUpdate)
        -> Result<(), ConnectionError>
    {
        unimplemented!();
        /*
        // Expand the window
        self.flow_control.expand_window(frame.size_increment())?;

        // Imediately apply the update
        self.flow_control.apply_window_update();

        Ok(())
        */
    }

    pub fn queue_frame(&mut self,
                       frame: Frame<B>,
                       stream: &mut store::Ptr<B>)
    {
        if self.queue_frame2(frame, stream) {
            // Notification required
            if let Some(ref task) = self.conn_task {
                task.notify();
            }
        }
    }

    /// Queue frame without actually notifying. Returns ture if the queue was
    /// succesfful.
    fn queue_frame2(&mut self, frame: Frame<B>, stream: &mut store::Ptr<B>)
        -> bool
    {
        self.buffered_data += frame.flow_len();

        // queue the frame in the buffer
        stream.pending_send.push_back(&mut self.buffer, frame);

        // Queue the stream
        !push_sender(&mut self.pending_send, stream)
    }

    /// Push the frame to the front of the stream's deque, scheduling the
    /// steream if needed.
    fn push_back_frame(&mut self, frame: Frame<B>, stream: &mut store::Ptr<B>) {
        // Push the frame to the front of the stream's deque
        stream.pending_send.push_front(&mut self.buffer, frame);

        // If needed, schedule the sender
        push_sender(&mut self.pending_capacity, stream);
    }

    pub fn poll_complete<T>(&mut self,
                            store: &mut Store<B>,
                            dst: &mut Codec<T, Prioritized<B>>)
        -> Poll<(), ConnectionError>
        where T: AsyncWrite,
    {
        // Track the task
        self.conn_task = Some(task::current());

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
                    // Figure out the byte size this frame applies to flow
                    // control
                    let len = cmp::min(frame.flow_len(), max_frame_len);

                    // Subtract the data size
                    self.buffered_data -= len;

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
                        return Ok(().into());
                    }

                    // No need to poll ready as poll_complete() does this for
                    // us...
                }
            }
        }
    }

    fn pop_frame(&mut self, store: &mut Store<B>, max_len: usize)
        -> Option<Frame<Prioritized<B>>>
    {
        loop {
            match self.pop_sender(store) {
                Some(mut stream) => {
                    let frame = match stream.pending_send.pop_front(&mut self.buffer).unwrap() {
                        Frame::Data(frame) => {
                            let len = frame.payload().remaining();

                            unimplemented!();
                            /*
                            if len > self.flow_control.effective_window_size() as usize {
                                // TODO: This could be smarter...
                                self.push_back_frame(frame.into(), &mut stream);

                                // Try again w/ the next stream
                                continue;
                            }
                            */

                            frame.into()
                        }
                        frame => frame,
                    };

                    if !stream.pending_send.is_empty() {
                        push_sender(&mut self.pending_send, &mut stream);
                    }

                    let frame = match frame {
                        Frame::Data(mut frame) => {
                            let eos = frame.is_end_stream();

                            if frame.payload().remaining() > max_len {
                                frame.unset_end_stream();
                            }

                            Frame::Data(frame.map(|buf| {
                                Prioritized {
                                    inner: buf.take(max_len),
                                    end_of_stream: eos,
                                    stream: stream.key(),
                                }
                            }))
                        }
                        frame => frame.map(|_| unreachable!()),
                    };

                    return Some(frame);
                }
                None => return None,
            }
        }
    }

    fn pop_sender<'a>(&mut self, store: &'a mut Store<B>) -> Option<store::Ptr<'a, B>> {
        // If the connection level window has capacity, pop off of the pending
        // capacity list first.

        unimplemented!();
        /*
        if self.flow_control.has_capacity() && !self.pending_capacity.is_empty() {
            let mut stream = self.pending_capacity
                .pop::<stream::Next>(store)
                .unwrap();

            stream.is_pending_send = false;
            Some(stream)
        } else {
            let stream = self.pending_send
                .pop::<stream::Next>(store);

            match stream {
                Some(mut stream) => {
                    stream.is_pending_send = false;
                    Some(stream)
                }
                None => None,
            }
        }
        */
    }

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
}

/// Push the stream onto the `pending_send` list. Returns true if the sender was
/// not already queued.
fn push_sender<B>(list: &mut store::List<B>, stream: &mut store::Ptr<B>)
    -> bool
{
    if stream.is_pending_send {
        return false;
    }

    list.push::<stream::Next>(stream);
    stream.is_pending_send = true;

    true
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
