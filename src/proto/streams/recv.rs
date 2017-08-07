use {client, frame, ConnectionError};
use proto::*;
use super::*;

use error::Reason::*;

use std::collections::VecDeque;
use std::marker::PhantomData;

#[derive(Debug)]
pub(super) struct Recv<P, B> {
    /// Maximum number of remote initiated streams
    max_streams: Option<usize>,

    /// Current number of remote initiated streams
    num_streams: usize,

    /// Initial window size of remote initiated streams
    init_window_sz: WindowSize,

    /// Connection level flow control governing received data
    flow_control: FlowControl,

    pending_window_updates: VecDeque<StreamId>,

    /// Holds frames that are waiting to be read
    buffer: Buffer<Bytes>,

    /// Refused StreamId, this represents a frame that must be sent out.
    refused: Option<StreamId>,

    _p: PhantomData<(P, B)>,
}

#[derive(Debug)]
pub(super) struct Chunk {
    /// Data frames pending receival
    pub pending_recv: buffer::Deque<Bytes>,
}

impl<P, B> Recv<P, B>
    where P: Peer,
          B: Buf,
{
    pub fn new(config: &Config) -> Self {
        Recv {
            max_streams: config.max_remote_initiated,
            num_streams: 0,
            init_window_sz: config.init_remote_window_sz,
            flow_control: FlowControl::new(config.init_remote_window_sz),
            pending_window_updates: VecDeque::new(),
            buffer: Buffer::new(),
            refused: None,
            _p: PhantomData,
        }
    }

    /// Update state reflecting a new, remotely opened stream
    ///
    /// Returns the stream state if successful. `None` if refused
    pub fn open(&mut self, id: StreamId) -> Result<Option<Stream<B>>, ConnectionError> {
        assert!(self.refused.is_none());

        try!(self.ensure_can_open(id));

        if let Some(max) = self.max_streams {
            if max <= self.num_streams {
                self.refused = Some(id);
                return Ok(None);
            }
        }

        // Increment the number of remote initiated streams
        self.num_streams += 1;

        Ok(Some(Stream::new()))
    }

    /// Transition the stream state based on receiving headers
    pub fn recv_headers(&mut self,
                        frame: frame::Headers,
                        stream: &mut store::Ptr<B>)
        -> Result<Option<frame::Headers>, ConnectionError>
    {
        stream.state.recv_open(self.init_window_sz, frame.is_end_stream())?;

        // Only servers can receive a headers frame that initiates the stream.
        // This is verified in `Streams` before calling this function.
        if P::is_server() {
            Ok(Some(frame))
        } else {
            // Push the frame onto the recv buffer
            stream.pending_recv.push_back(&mut self.buffer, frame.into());
            stream.notify_recv();

            Ok(None)
        }
    }

    pub fn recv_eos(&mut self, stream: &mut Stream<B>)
        -> Result<(), ConnectionError>
    {
        stream.state.recv_close()
    }

    pub fn recv_data(&mut self,
                     frame: frame::Data,
                     stream: &mut Stream<B>)
        -> Result<(), ConnectionError>
    {
        let sz = frame.payload().len();

        if sz > MAX_WINDOW_SIZE as usize {
            unimplemented!();
        }

        let sz = sz as WindowSize;

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

        if frame.is_end_stream() {
            try!(stream.state.recv_close());
        }

        // Push the frame onto the recv buffer
        stream.pending_recv.push_back(&mut self.buffer, frame.into());
        stream.notify_recv();

        Ok(())
    }

    pub fn dec_num_streams(&mut self) {
        self.num_streams -= 1;
    }

    /// Returns true if the remote peer can initiate a stream with the given ID.
    fn ensure_can_open(&self, id: StreamId) -> Result<(), ConnectionError> {
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

    /// Send any pending refusals.
    pub fn send_pending_refusal<T>(&mut self, dst: &mut Codec<T, B>)
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
        // TODO: handle overflow
        self.flow_control.expand_window(sz);

        Ok(())
    }

    pub fn expand_stream_window(&mut self,
                                id: StreamId,
                                sz: WindowSize,
                                stream: &mut Stream<B>)
        -> Result<(), ConnectionError>
    {
        // TODO: handle overflow
        if let Some(flow) = stream.recv_flow_control() {
            flow.expand_window(sz);
            self.pending_window_updates.push_back(id);
        }

        Ok(())
    }

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

    /// Send stream level window update
    pub fn send_stream_window_update<T>(&mut self,
                                        streams: &mut Store<B>,
                                        dst: &mut Codec<T, B>)
        -> Poll<(), ConnectionError>
        where T: AsyncWrite,
    {
        while let Some(id) = self.pending_window_updates.pop_front() {
            let flow = streams.find_mut(&id)
                .and_then(|stream| stream.recv_flow_control());


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

    fn reset(&mut self, _stream_id: StreamId, _reason: Reason) {
        unimplemented!();
    }
}

impl<B> Recv<client::Peer, B>
    where B: Buf,
{
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
                stream.recv_task = Some(task::current());
                Ok(Async::NotReady)
            }
        }
    }
}
