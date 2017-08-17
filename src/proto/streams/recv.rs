use {client, server, frame, ConnectionError};
use proto::*;
use super::*;

use error::Reason::*;

use std::collections::VecDeque;
use std::marker::PhantomData;

#[derive(Debug)]
pub(super) struct Recv<B> {
    /// Maximum number of remote initiated streams
    max_streams: Option<usize>,

    /// Current number of remote initiated streams
    num_streams: usize,

    /// Initial window size of remote initiated streams
    init_window_sz: WindowSize,

    /// Connection level flow control governing received data
    flow_control: FlowControl,

    /// The lowest stream ID that is still idle
    next_stream_id: StreamId,

    /// Streams that have pending window updates
    /// TODO: don't use a VecDeque
    pending_window_updates: VecDeque<StreamId>,

    /// New streams to be accepted
    pending_accept: store::List<B>,

    /// Holds frames that are waiting to be read
    buffer: Buffer<Bytes>,

    /// Refused StreamId, this represents a frame that must be sent out.
    refused: Option<StreamId>,

    _p: PhantomData<(B)>,
}

#[derive(Debug)]
pub(super) struct Chunk {
    /// Data frames pending receival
    pub pending_recv: buffer::Deque<Bytes>,
}

#[derive(Debug, Clone, Copy)]
struct Indices {
    head: store::Key,
    tail: store::Key,
}

impl<B> Recv<B> where B: Buf {
    pub fn new<P: Peer>(config: &Config) -> Self {
        let next_stream_id = if P::is_server() {
            1
        } else {
            2
        };

        Recv {
            max_streams: config.max_remote_initiated,
            num_streams: 0,
            init_window_sz: config.init_remote_window_sz,
            flow_control: FlowControl::with_window_size(config.init_remote_window_sz),
            next_stream_id: next_stream_id.into(),
            pending_window_updates: VecDeque::new(),
            pending_accept: store::List::new(),
            buffer: Buffer::new(),
            refused: None,
            _p: PhantomData,
        }
    }

    /// Update state reflecting a new, remotely opened stream
    ///
    /// Returns the stream state if successful. `None` if refused
    pub fn open<P: Peer>(&mut self, id: StreamId)
        -> Result<Option<Stream<B>>, ConnectionError>
    {
        assert!(self.refused.is_none());

        try!(self.ensure_can_open::<P>(id));

        if !self.can_inc_num_streams() {
            self.refused = Some(id);
            return Ok(None);
        }

        Ok(Some(Stream::new(id)))
    }

    pub fn take_request(&mut self, stream: &mut store::Ptr<B>)
        -> Result<Request<()>, ConnectionError>
    {
        match stream.pending_recv.pop_front(&mut self.buffer) {
            Some(Frame::Headers(frame)) => {
                // TODO: This error should probably be caught on receipt of the
                // frame vs. now.
                Ok(server::Peer::convert_poll_message(frame)?)
            }
            _ => panic!(),
        }
    }

    pub fn poll_response(&mut self, stream: &mut store::Ptr<B>)
        -> Poll<Response<()>, ConnectionError> {
        // If the buffer is not empty, then the first frame must be a HEADERS
        // frame or the user violated the contract.
        match stream.pending_recv.pop_front(&mut self.buffer) {
            Some(Frame::Headers(v)) => {
                // TODO: This error should probably be caught on receipt of the
                // frame vs. now.
                Ok(client::Peer::convert_poll_message(v)?.into())
            }
            Some(frame) => unimplemented!(),
            None => {
                stream.state.ensure_recv_open()?;

                stream.recv_task = Some(task::current());
                Ok(Async::NotReady)
            }
        }
    }

    /// Transition the stream state based on receiving headers
    pub fn recv_headers<P: Peer>(&mut self,
                                 frame: frame::Headers,
                                 stream: &mut store::Ptr<B>)
        -> Result<(), ConnectionError>
    {
        trace!("opening stream; init_window={}", self.init_window_sz);
        let is_initial = stream.state.recv_open(frame.is_end_stream())?;

        // TODO: Update flow control

        if is_initial {
            if !self.can_inc_num_streams() {
                unimplemented!();
            }

            if frame.stream_id() >= self.next_stream_id {
                self.next_stream_id = frame.stream_id();
                self.next_stream_id.increment();
            } else {
                return Err(ProtocolError.into());
            }

            // Increment the number of concurrent streams
            self.inc_num_streams();
        }

        // Push the frame onto the stream's recv buffer
        stream.pending_recv.push_back(&mut self.buffer, frame.into());
        stream.notify_recv();

        // Only servers can receive a headers frame that initiates the stream.
        // This is verified in `Streams` before calling this function.
        if P::is_server() {
            self.pending_accept.push::<stream::Next>(stream);
        }

        Ok(())
    }

    /// Transition the stream based on receiving trailers
    pub fn recv_trailers<P: Peer>(&mut self,
                                  frame: frame::Headers,
                                  stream: &mut store::Ptr<B>)
        -> Result<(), ConnectionError>
    {
        // Transition the state
        stream.state.recv_close();

        // Push the frame onto the stream's recv buffer
        stream.pending_recv.push_back(&mut self.buffer, frame.into());
        stream.notify_recv();

        Ok(())
    }

    pub fn recv_data(&mut self,
                     frame: frame::Data,
                     stream: &mut store::Ptr<B>)
        -> Result<(), ConnectionError>
    {
        let sz = frame.payload().len();

        if sz > MAX_WINDOW_SIZE as usize {
            unimplemented!();
        }

        let sz = sz as WindowSize;

        // TODO: implement
        /*
        match stream.recv_flow_control() {
            Some(flow) => {
                // Ensure there's enough capacity on the connection before
                // acting on the stream.
                try!(self.flow_control.ensure_window(sz, FlowControlError));

                // Claim the window on the stream
                try!(flow.claim_window(sz, FlowControlError));

                // Claim the window on the connection.
                self.flow_control.claim_window(sz, FlowControlError)
                    .expect("local connection flow control error");
            }
            None => return Err(ProtocolError.into()),
        }
        */

        if frame.is_end_stream() {
            try!(stream.state.recv_close());
        }

        // Push the frame onto the recv buffer
        stream.pending_recv.push_back(&mut self.buffer, frame.into());
        stream.notify_recv();

        Ok(())
    }

    pub fn recv_push_promise<P: Peer>(&mut self,
                                      frame: frame::PushPromise,
                                      stream: store::Key,
                                      store: &mut Store<B>)
        -> Result<(), ConnectionError>
    {
        // First, make sure that the values are legit
        self.ensure_can_reserve::<P>(frame.promised_id())?;

        // Make sure that the stream state is valid
        store[stream].state.ensure_recv_open()?;

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
        let mut new_stream = Stream::new(frame.promised_id());
        new_stream.state.reserve_remote();

        let mut ppp = store[stream].pending_push_promises.take();

        {
            // Store the stream
            let mut new_stream = store
                .insert(frame.promised_id(), new_stream);

            ppp.push::<stream::Next>(&mut new_stream);
        }

        let stream = &mut store[stream];

        stream.pending_push_promises = ppp;
        stream.notify_recv();

        Ok(())
    }

    pub fn ensure_not_idle(&self, id: StreamId) -> Result<(), ConnectionError> {
        if id >= self.next_stream_id {
            return Err(ProtocolError.into());
        }

        Ok(())
    }

    pub fn recv_reset(&mut self, frame: frame::Reset, stream: &mut Stream<B>)
        -> Result<(), ConnectionError>
    {
        let err = ConnectionError::Proto(frame.reason());

        // Notify the stream
        stream.state.recv_err(&err);
        stream.notify_recv();
        Ok(())
    }

    pub fn recv_err(&mut self, err: &ConnectionError, stream: &mut Stream<B>) {
        // Receive an error
        stream.state.recv_err(err);

        // If a receiver is waiting, notify it
        stream.notify_recv();
    }

    /// Returns true if the current stream concurrency can be incremetned
    fn can_inc_num_streams(&self) -> bool {
        if let Some(max) = self.max_streams {
            max > self.num_streams
        } else {
            true
        }
    }

    /// Increments the number of concurrenty streams. Panics on failure as this
    /// should have been validated before hand.
    fn inc_num_streams(&mut self) {
        if !self.can_inc_num_streams() {
            panic!();
        }

        // Increment the number of remote initiated streams
        self.num_streams += 1;
    }

    pub fn dec_num_streams(&mut self) {
        self.num_streams -= 1;
    }

    /// Returns true if the remote peer can initiate a stream with the given ID.
    fn ensure_can_open<P: Peer>(&self, id: StreamId)
        -> Result<(), ConnectionError>
    {
        if !P::is_server() {
            // Remote is a server and cannot open streams. PushPromise is
            // registered by reserving, so does not go through this path.
            return Err(ProtocolError.into());
        }

        // Ensure that the ID is a valid server initiated ID
        if !id.is_client_initiated() {
            return Err(ProtocolError.into());
        }

        Ok(())
    }

    /// Returns true if the remote peer can reserve a stream with the given ID.
    fn ensure_can_reserve<P: Peer>(&self, promised_id: StreamId)
        -> Result<(), ConnectionError>
    {
        // TODO: Are there other rules?
        if P::is_server() {
            // The remote is a client and cannot reserve
            return Err(ProtocolError.into());
        }

        if !promised_id.is_server_initiated() {
            return Err(ProtocolError.into());
        }

        Ok(())
    }

    /// Send any pending refusals.
    pub fn send_pending_refusal<T>(&mut self, dst: &mut Codec<T, Prioritized<B>>)
        -> Poll<(), ConnectionError>
        where T: AsyncWrite,
    {
        if let Some(stream_id) = self.refused.take() {
            let frame = frame::Reset::new(stream_id, RefusedStream);

            match dst.start_send(frame.into())? {
                AsyncSink::Ready => {
                    self.reset(stream_id, RefusedStream);
                    return Ok(Async::Ready(()));
                }
                AsyncSink::NotReady(_) => {
                    self.refused = Some(stream_id);
                    return Ok(Async::NotReady);
                }
            }
        }

        Ok(Async::Ready(()))
    }

    pub fn expand_connection_window(&mut self, sz: WindowSize)
        -> Result<(), ConnectionError>
    {
        unimplemented!();
        /*
        // TODO: handle overflow
        self.flow_control.expand_window(sz);

        Ok(())
        */
    }

    pub fn expand_stream_window(&mut self,
                                id: StreamId,
                                sz: WindowSize,
                                stream: &mut store::Ptr<B>)
        -> Result<(), ConnectionError>
    {
        unimplemented!();
        /*
        // TODO: handle overflow
        if let Some(flow) = stream.recv_flow_control() {
            flow.expand_window(sz);
            self.pending_window_updates.push_back(id);
        }

        Ok(())
        */
    }

    /*
    /// Send connection level window update
    pub fn send_connection_window_update<T>(&mut self, dst: &mut Codec<T, B>)
        -> Poll<(), ConnectionError>
        where T: AsyncWrite,
    {
        if let Some(incr) = self.flow_control.peek_window_update() {
            let frame = frame::WindowUpdate::new(StreamId::zero(), incr);

            if dst.start_send(frame.into())?.is_ready() {
                assert_eq!(Some(incr), self.flow_control.apply_window_update());
            } else {
                return Ok(Async::NotReady);
            }
        }

        Ok(().into())
    }
    */

    pub fn next_incoming(&mut self, store: &mut Store<B>) -> Option<store::Key> {
        self.pending_accept.pop::<stream::Next>(store)
            .map(|ptr| ptr.key())
    }

    pub fn poll_chunk(&mut self, stream: &mut Stream<B>)
        -> Poll<Option<Chunk>, ConnectionError>
    {
        let frames = stream.pending_recv
            .take_while(&mut self.buffer, |frame| frame.is_data());

        if frames.is_empty() {
            if stream.state.is_recv_closed() {
                Ok(None.into())
            } else {
                stream.recv_task = Some(task::current());
                Ok(Async::NotReady)
            }
        } else {
            Ok(Some(Chunk {
                pending_recv: frames,
            }).into())
        }
    }

    pub fn pop_bytes(&mut self, chunk: &mut Chunk) -> Option<Bytes> {
        match chunk.pending_recv.pop_front(&mut self.buffer) {
            Some(Frame::Data(frame)) => {
                Some(frame.into_payload())
            }
            None => None,
            _ => panic!("unexpected frame type"),
        }
    }

    /*
    /// Send stream level window update
    pub fn send_stream_window_update<T>(&mut self,
                                        streams: &mut Store<B>,
                                        dst: &mut Codec<T, B>)
        -> Poll<(), ConnectionError>
        where T: AsyncWrite,
    {
        while let Some(id) = self.pending_window_updates.pop_front() {
            let flow = streams.find_mut(&id)
                .and_then(|stream| stream.into_mut().recv_flow_control());


            if let Some(flow) = flow {
                if let Some(incr) = flow.peek_window_update() {
                    let frame = frame::WindowUpdate::new(id, incr);

                    if dst.start_send(frame.into())?.is_ready() {
                        assert_eq!(Some(incr), flow.apply_window_update());
                    } else {
                        self.pending_window_updates.push_front(id);
                        return Ok(Async::NotReady);
                    }
                }
            }
        }

        Ok(().into())
    }
    */

    fn reset(&mut self, _stream_id: StreamId, _reason: Reason) {
        unimplemented!();
    }
}
