use {frame, Peer, ConnectionError};
use proto::*;
use super::{state, Config, Store};

use error::User::*;

use bytes::Buf;

use std::collections::VecDeque;
use std::marker::PhantomData;

#[derive(Debug)]
pub struct Send<P> {
    /// Maximum number of locally initiated streams
    max_streams: Option<usize>,

    /// Current number of locally initiated streams
    num_streams: usize,

    /// Initial window size of locally initiated streams
    init_window_sz: WindowSize,

    /// Connection level flow control governing sent data
    flow_control: state::FlowControl,

    /// Holds the list of streams on which local window updates may be sent.
    // XXX It would be cool if this didn't exist.
    pending_window_updates: VecDeque<StreamId>,

    /// When `poll_window_update` is not ready, then the calling task is saved to
    /// be notified later. Access to poll_window_update must not be shared across tasks,
    /// as we only track a single task (and *not* i.e. a task per stream id).
    blocked: Option<task::Task>,

    _p: PhantomData<P>,
}

impl<P: Peer> Send<P> {
    pub fn new(config: &Config) -> Self {
        Send {
            max_streams: config.max_local_initiated,
            num_streams: 0,
            init_window_sz: config.init_local_window_sz,
            flow_control: state::FlowControl::new(config.init_local_window_sz),
            pending_window_updates: VecDeque::new(),
            blocked: None,
            _p: PhantomData,
        }
    }

    /// Update state reflecting a new, locally opened stream
    ///
    /// Returns the stream state if successful. `None` if refused
    pub fn open(&mut self, id: StreamId) -> Result<state::Stream, ConnectionError> {
        try!(self.ensure_can_open(id));

        if let Some(max) = self.max_streams {
            if max <= self.num_streams {
                return Err(Rejected.into());
            }
        }

        // Increment the number of locally initiated streams
        self.num_streams += 1;

        Ok(state::Stream::default())
    }

    pub fn send_headers(&mut self, state: &mut state::Stream, eos: bool)
        -> Result<(), ConnectionError>
    {
        state.send_open(self.init_window_sz, eos)
    }

    pub fn send_eos(&mut self, state: &mut state::Stream)
        -> Result<(), ConnectionError>
    {
        state.send_close()
    }

    pub fn send_data<B: Buf>(&mut self,
                             frame: &frame::Data<B>,
                             state: &mut state::Stream)
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
            match state.send_flow_control() {
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

            if state.is_closed() {
                return Err(InactiveStreamId.into())
            } else {
                return Err(UnexpectedFrameType.into())
            }
        }

        if frame.is_end_stream() {
            try!(state.send_close());
        }

        Ok(())
    }

    /// Get pending window updates
    pub fn poll_window_update(&mut self, streams: &mut Store)
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
                streams.get_mut(&id)
                    .and_then(|state| state.send_flow_control())
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
                                     state: &mut state::Stream)
        -> Result<(), ConnectionError>
    {
        if let Some(flow) = state.send_flow_control() {
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
    fn ensure_can_open(&self, id: StreamId) -> Result<(), ConnectionError> {
        if P::is_server() {
            // Servers cannot open streams. PushPromise must first be reserved.
            return Err(UnexpectedFrameType.into());
        }

        if !id.is_client_initiated() {
            return Err(InvalidStreamId.into());
        }

        Ok(())
    }
}
