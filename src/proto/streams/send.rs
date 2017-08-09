use {frame, ConnectionError};
use proto::*;
use super::*;

use error::User::*;

use bytes::Buf;

use std::collections::VecDeque;
use std::marker::PhantomData;

/// Manages state transitions related to outbound frames.
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

    prioritize: Prioritize<B>,
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
            prioritize: Prioritize::new(config),
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
            let unadvertised = stream.unadvertised_send_window;

            match stream.send_flow_control() {
                Some(flow) => {
                    // Ensure that the size fits within the advertised size
                    try!(flow.ensure_window(
                            sz + unadvertised, FlowControlViolation));

                    // Now, claim the window on the stream
                    flow.claim_window(sz, FlowControlViolation)
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

    /*
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
    */

    pub fn recv_connection_window_update(&mut self, frame: frame::WindowUpdate)
        -> Result<(), ConnectionError>
    {
        self.priority.recv_window_update(frame)?;

        // TODO: If there is available connection capacity, release pending
        // streams.

        Ok(())
    }

    pub fn recv_stream_window_update(&mut self,
                                     frame: frame::WindowUpdate,
                                     stream: &mut store::Ptr<B>)
        -> Result<(), ConnectionError>
    {
        unimplemented!();
        /*
        if let Some(flow) = stream.send_flow_control() {
            // TODO: Handle invalid increment
            flow.expand_window(frame.size_increment());
        }

        if let Some(task) = self.blocked.take() {
            task.notify();
        }

        Ok(())
        */
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
