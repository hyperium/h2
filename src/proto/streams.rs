use {frame, Peer, StreamId, FrameSize, ConnectionError};
use proto::*;
use error::Reason::*;

use ordermap::{OrderMap, Entry};

use std::collections::VecDeque;

// TODO: All the VecDeques should become linked lists using the state::Stream
// values.
#[derive(Debug)]
pub struct Streams {
    /// State related to managing the set of streams.
    inner: Inner,

    /// Streams
    streams: OrderMap<StreamId, state::Stream>,

    /// Refused StreamId, this represents a frame that must be sent out.
    refused: Option<StreamId>,
}

/// Fields needed to manage state related to managing the set of streams. This
/// is mostly split out to make ownership happy.
///
/// TODO: better name
#[derive(Debug)]
struct Inner {
    /// True when running in context of an H2 server
    is_server: bool,

    /// Maximum number of remote initiated streams
    max_remote_initiated: Option<usize>,

    /// Current number of remote initiated streams
    num_remote_initiated: usize,

    /// Initial window size of remote initiated streams
    init_remote_window_sz: usize,

    /// Maximum number of locally initiated streams
    max_local_initiated: Option<usize>,

    /// Current number of locally initiated streams
    num_local_initiated: usize,

    /// Initial window size of locally initiated streams
    init_local_window_sz: usize,

    /// Connection level flow control governing received data
    recv_flow_control: state::FlowControl,

    /// Connection level flow control governing sent data
    send_flow_control: state::FlowControl,
}

#[derive(Debug)]
pub struct Config {
    /// Maximum number of remote initiated streams
    pub max_remote_initiated: Option<usize>,

    /// Initial window size of remote initiated streams
    pub init_remote_window_sz: usize,

    /// Maximum number of locally initiated streams
    pub max_local_initiated: Option<usize>,

    /// Initial window size of locally initiated streams
    pub init_local_window_sz: usize,
}

impl Streams {
    pub fn new<P: Peer>(config: Config) -> Self {
        Streams {
            inner: Inner {
                is_server: P::is_server(),
                max_remote_initiated: config.max_remote_initiated,
                num_remote_initiated: 0,
                init_remote_window_sz: config.init_remote_window_sz,
                max_local_initiated: config.max_local_initiated,
                num_local_initiated: 0,
                init_local_window_sz: config.init_local_window_sz,
                recv_flow_control: state::FlowControl::new(config.init_remote_window_sz),
                send_flow_control: state::FlowControl::new(config.init_local_window_sz),
            },
            streams: OrderMap::default(),
            refused: None,
        }
    }

    pub fn recv_headers(&mut self, frame: frame::Headers)
        -> Result<Option<frame::Headers>, ConnectionError>
    {
        let id = frame.stream_id();

        try!(validate_stream_id(id));

        let state = match self.streams.entry(id) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                // Trailers cannot open a stream. Trailers are header frames
                // that do not contain pseudo headers. Requests MUST contain a
                // method and responses MUST contain a status. If they do not,t
                // hey are considered to be malformed.
                if frame.is_trailers() {
                    return Err(ProtocolError.into());
                }

                if let Some(state) = try!(self.inner.remote_open(id)) {
                    e.insert(state)
                } else {
                    // Stream is refused
                    assert!(self.refused.is_none());
                    self.refused = Some(id);
                    return Ok(None);
                }
            }
        };

        if frame.is_trailers() {
            try!(self.inner.recv_trailers(id, state, frame.is_end_stream()));
        } else {
            try!(self.inner.recv_headers(id, state, frame.is_end_stream()));
        }

        Ok(Some(frame))
    }

    pub fn recv_data(&mut self, frame: &frame::Data)
        -> Result<(), ConnectionError>
    {
        let id = frame.stream_id();
        let sz = frame.payload().len();

        let state = match self.streams.get_mut(&id) {
            Some(state) => state,
            None => return Err(ProtocolError.into()),
        };

        // Ensure there's enough capacity on the connection before acting on the
        // stream.
        self.inner.recv_data(id, state, sz, frame.is_end_stream())
    }

    pub fn recv_reset(&mut self, frame: &frame::Reset)
        -> Result<(), ConnectionError>
    {
        unimplemented!();
    }

    pub fn recv_window_update(&mut self, frame: frame::WindowUpdate) {
        unimplemented!();
    }

    pub fn recv_push_promise(&mut self, frame: frame::PushPromise) {
        unimplemented!();
    }

    /// Send any pending refusals.
    pub fn send_refuse<T, B>(&mut self, dst: &mut Codec<T, B>) -> Poll<(), ConnectionError>
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

    /// Reset a stream
    fn reset(&mut self, stream_id: StreamId, reason: Reason) {
        unimplemented!();
    }
}

impl Inner {
    /// Update state reflecting a new, remotely opened stream
    ///
    /// Returns the stream state if successful. `None` if refused
    fn remote_open(&mut self, id: StreamId) -> Result<Option<state::Stream>, ConnectionError> {
        if !self.can_remote_open(id) {
            return Err(ProtocolError.into());
        }

        if let Some(max) = self.max_remote_initiated {
            if max <= self.num_remote_initiated {
                return Ok(None);
            }
        }

        // Increment the number of remote initiated streams
        self.num_remote_initiated += 1;

        Ok(Some(state::Stream::default()))
    }

    /// Transition the stream state based on receiving headers
    fn recv_headers(&mut self, id: StreamId, state: &mut state::Stream, eos: bool)
        -> Result<(), ConnectionError>
    {
        try!(state.recv_open(self.init_remote_window_sz, eos));

        if state.is_closed() {
            self.stream_closed(id);
        }

        Ok(())
    }

    fn recv_trailers(&mut self, id: StreamId, state: &mut state::Stream, eos: bool)
        -> Result<(), ConnectionError>
    {
        unimplemented!();
    }

    fn recv_data(&mut self, id: StreamId, state: &mut state::Stream, sz: usize, eos: bool)
        -> Result<(), ConnectionError>
    {
        match state.recv_flow_control() {
            Some(flow) => {
                // Ensure there's enough capacity on the connection before
                // acting on the stream.
                try!(self.recv_flow_control.ensure_window(sz));

                // Claim the window on the stream
                try!(flow.claim_window(sz));

                // Claim the window on the connection.
                self.recv_flow_control.claim_window(sz)
                    .expect("local connection flow control error");
            }
            None => return Err(ProtocolError.into()),
        }

        state.recv_close();

        if state.is_closed() {
            self.stream_closed(id)
        }

        Ok(())
    }

    fn stream_closed(&mut self, id: StreamId) {
        if self.is_local_init(id) {
            self.num_local_initiated -= 1;
        } else {
            self.num_remote_initiated -= 1;
        }
    }

    fn is_local_init(&self, id: StreamId) -> bool {
        assert!(!id.is_zero());
        self.is_server == id.is_server_initiated()
    }

    /// Returns true if the remote peer can initiate a stream with the given ID.
    fn can_remote_open(&self, id: StreamId) -> bool {
        if self.is_server {
            // Remote is a client and cannot open streams
            return false;
        }

        // Ensure that the ID is a valid server initiated ID
        id.is_server_initiated()
    }
}

/// Ensures non-zero stream ID
fn validate_stream_id(id: StreamId) -> Result<(), ConnectionError> {
    if id.is_zero() {
        Err(ProtocolError.into())
    } else {
        Ok(())
    }
}
