use {frame, Peer, ConnectionError};
use proto::*;
use super::*;

use error::Reason::*;

use std::collections::VecDeque;
use std::marker::PhantomData;

#[derive(Debug)]
pub struct Recv<P> {
    /// Maximum number of remote initiated streams
    max_streams: Option<usize>,

    /// Current number of remote initiated streams
    num_streams: usize,

    /// Initial window size of remote initiated streams
    init_window_sz: WindowSize,

    /// Connection level flow control governing received data
    flow_control: FlowControl,

    pending_window_updates: VecDeque<StreamId>,

    /// Refused StreamId, this represents a frame that must be sent out.
    refused: Option<StreamId>,

    _p: PhantomData<P>,
}

impl<P: Peer> Recv<P> {
    pub fn new(config: &Config) -> Self {
        Recv {
            max_streams: config.max_remote_initiated,
            num_streams: 0,
            init_window_sz: config.init_remote_window_sz,
            flow_control: FlowControl::new(config.init_remote_window_sz),
            pending_window_updates: VecDeque::new(),
            refused: None,
            _p: PhantomData,
        }
    }

    /// Update state reflecting a new, remotely opened stream
    ///
    /// Returns the stream state if successful. `None` if refused
    pub fn open(&mut self, id: StreamId) -> Result<Option<State>, ConnectionError> {
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

        Ok(Some(State::default()))
    }

    /// Transition the stream state based on receiving headers
    pub fn recv_headers(&mut self, state: &mut State, eos: bool)
        -> Result<(), ConnectionError>
    {
        state.recv_open(self.init_window_sz, eos)
    }

    pub fn recv_eos(&mut self, state: &mut State)
        -> Result<(), ConnectionError>
    {
        state.recv_close()
    }

    pub fn recv_data(&mut self,
                     frame: &frame::Data,
                     state: &mut State)
        -> Result<(), ConnectionError>
    {
        let sz = frame.payload().len();

        if sz > MAX_WINDOW_SIZE as usize {
            unimplemented!();
        }

        let sz = sz as WindowSize;

        match state.recv_flow_control() {
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
            try!(state.recv_close());
        }

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
    pub fn send_pending_refusal<T, B>(&mut self, dst: &mut Codec<T, B>)
        -> Poll<(), ConnectionError>
        where T: AsyncWrite,
              B: Buf,
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
                                state: &mut State)
        -> Result<(), ConnectionError>
    {
        // TODO: handle overflow
        if let Some(flow) = state.recv_flow_control() {
            flow.expand_window(sz);
            self.pending_window_updates.push_back(id);
        }

        Ok(())
    }

    /// Send connection level window update
    pub fn send_connection_window_update<T, B>(&mut self, dst: &mut Codec<T, B>)
        -> Poll<(), ConnectionError>
        where T: AsyncWrite,
              B: Buf,
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

    /// Send stream level window update
    pub fn send_stream_window_update<T, B>(&mut self,
                                           streams: &mut Store,
                                           dst: &mut Codec<T, B>)
        -> Poll<(), ConnectionError>
        where T: AsyncWrite,
              B: Buf,
    {
        while let Some(id) = self.pending_window_updates.pop_front() {
            let flow = streams.get_mut(&id)
                .and_then(|state| state.recv_flow_control());


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
