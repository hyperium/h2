use {frame, ConnectionError};
use proto::*;
use super::*;

use error::User::*;

use bytes::Buf;

use std::collections::VecDeque;
use std::marker::PhantomData;

#[derive(Debug)]
pub(super) struct Send<B> {
    /// Maximum number of locally initiated streams
    max_streams: Option<usize>,

    /// Current number of locally initiated streams
    num_streams: usize,

    /// Stream identifier to use for next initialized stream.
    next_stream_id: StreamId,

    /// Initial window size of locally initiated streams
    init_window_sz: WindowSize,

    /// Connection level flow control governing sent data
    flow_control: FlowControl,

    /// Holds the list of streams on which local window updates may be sent.
    // XXX It would be cool if this didn't exist.
    pending_window_updates: VecDeque<StreamId>,

    prioritize: Prioritize<B>,

    /// When `poll_window_update` is not ready, then the calling task is saved to
    /// be notified later. Access to poll_window_update must not be shared across tasks,
    /// as we only track a single task (and *not* i.e. a task per stream id).
    blocked: Option<task::Task>,
}

impl<B> Send<B> where B: Buf {

    /// Create a new `Send`
    pub fn new<P: Peer>(config: &Config) -> Self {
        let next_stream_id = if P::is_server() {
            2
        } else {
            1
        };

        Send {
            max_streams: config.max_local_initiated,
            num_streams: 0,
            next_stream_id: next_stream_id.into(),
            init_window_sz: config.init_local_window_sz,
            flow_control: FlowControl::new(config.init_local_window_sz),
            prioritize: Prioritize::new(),
            pending_window_updates: VecDeque::new(),
            blocked: None,
        }
    }

    /// Update state reflecting a new, locally opened stream
    ///
    /// Returns the stream state if successful. `None` if refused
    pub fn open<P: Peer>(&mut self)
        -> Result<Stream<B>, ConnectionError>
    {
        try!(self.ensure_can_open::<P>());

        if let Some(max) = self.max_streams {
            if max <= self.num_streams {
                return Err(Rejected.into());
            }
        }

        let ret = Stream::new(self.next_stream_id);

        // Increment the number of locally initiated streams
        self.num_streams += 1;
        self.next_stream_id.increment();

        Ok(ret)
    }

    pub fn send_headers(&mut self,
                        frame: frame::Headers,
                        stream: &mut store::Ptr<B>)
        -> Result<(), ConnectionError>
    {
        // Update the state
        stream.state.send_open(self.init_window_sz, frame.is_end_stream())?;

        // Queue the frame for sending
        self.prioritize.queue_frame(frame.into(), stream);

        Ok(())
    }

    pub fn send_eos(&mut self, stream: &mut Stream<B>)
        -> Result<(), ConnectionError>
    {
        stream.state.send_close()
    }

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

        // Make borrow checker happy
        loop {
            match stream.send_flow_control() {
                Some(flow) => {
                    try!(self.flow_control.ensure_window(sz, FlowControlViolation));

                    // Claim the window on the stream
                    try!(flow.claim_window(sz, FlowControlViolation));

                    // Claim the window on the connection
                    self.flow_control.claim_window(sz, FlowControlViolation)
                        .expect("local connection flow control error");

                    break;
                }
                None => {}
            }

            if stream.state.is_closed() {
                return Err(InactiveStreamId.into())
            } else {
                return Err(UnexpectedFrameType.into())
            }
        }

        if frame.is_end_stream() {
            try!(stream.state.send_close());
        }

        self.prioritize.queue_frame(frame.into(), stream);

        Ok(())
    }

    pub fn poll_complete<T>(&mut self,
                            store: &mut Store<B>,
                            dst: &mut Codec<T, B>)
        -> Poll<(), ConnectionError>
        where T: AsyncWrite,
    {
        self.prioritize.poll_complete(store, dst)
    }

    /// Get pending window updates
    pub fn poll_window_update(&mut self, streams: &mut Store<B>)
        -> Poll<WindowUpdate, ConnectionError>
    {
        // This biases connection window updates, which probably makes sense.
        //
        // TODO: We probably don't want to expose connection level updates
        if let Some(incr) = self.flow_control.apply_window_update() {
            return Ok(Async::Ready(WindowUpdate::new(StreamId::zero(), incr)));
        }

        // TODO this should probably account for stream priority?
        let update = self.pending_window_updates.pop_front()
            .and_then(|id| {
                streams.find_mut(&id)
                    .and_then(|stream| stream.into_mut().send_flow_control())
                    .and_then(|flow| flow.apply_window_update())
                    .map(|incr| WindowUpdate::new(id, incr))
            });

        if let Some(update) = update {
            return Ok(Async::Ready(update));
        }

        // Update the task.
        //
        // TODO: Extract this "gate" logic
        self.blocked = Some(task::current());

        return Ok(Async::NotReady);
    }

    pub fn recv_connection_window_update(&mut self, frame: frame::WindowUpdate)
        -> Result<(), ConnectionError>
    {
        // TODO: Handle invalid increment
        self.flow_control.expand_window(frame.size_increment());

        if let Some(task) = self.blocked.take() {
            task.notify();
        }

        Ok(())
    }

    pub fn recv_stream_window_update(&mut self,
                                     frame: frame::WindowUpdate,
                                     stream: &mut store::Ptr<B>)
        -> Result<(), ConnectionError>
    {
        if let Some(flow) = stream.send_flow_control() {
            // TODO: Handle invalid increment
            flow.expand_window(frame.size_increment());
        }

        if let Some(task) = self.blocked.take() {
            task.notify();
        }

        Ok(())
    }

    pub fn dec_num_streams(&mut self) {
        self.num_streams -= 1;
    }

    /// Returns true if the local actor can initiate a stream with the given ID.
    fn ensure_can_open<P: Peer>(&self) -> Result<(), ConnectionError> {
        if P::is_server() {
            // Servers cannot open streams. PushPromise must first be reserved.
            return Err(UnexpectedFrameType.into());
        }

        // TODO: Handle StreamId overflow

        Ok(())
    }
}
