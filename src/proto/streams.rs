use {frame, Peer, StreamId, ConnectionError};
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

    /// Flow control logic handler
    flow_control: FlowControl,

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

    /// Current number of remote initiated streams
    num_remote_initiated: usize,

    /// Current number of locally initiated streams
    num_local_initiated: usize,

    /// Configuration options
    config: Config,
}

#[derive(Debug)]
pub struct Config {
    /// Maximum number of remote initiated streams
    pub max_remote_initiated: Option<usize>,

    /// Initial window size of remote initiated streams
    pub init_remote_window_sz: WindowSize,

    /// Maximum number of locally initiated streams
    pub max_local_initiated: Option<usize>,

    /// Initial window size of locally initiated streams
    pub init_local_window_sz: WindowSize,
}

impl Streams {
    pub fn new<P: Peer>(config: Config) -> Self {
        Streams {
            inner: Inner {
                is_server: P::is_server(),
                num_remote_initiated: 0,
                num_local_initiated: 0,
                config: config,
            },
            streams: OrderMap::default(),
            flow_control: FlowControl::new(),
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

        try!(self.inner.recv_headers(state));

        Ok(Some(frame))
    }

    pub fn recv_data(&mut self, frame: &frame::Data)
        -> Result<(), ConnectionError>
    {
        unimplemented!();
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

        if let Some(max) = self.config.max_remote_initiated {
            if max <= self.num_remote_initiated {
                return Ok(None);
            }
        }

        // Increment the number of remote initiated streams
        self.num_remote_initiated += 1;

        Ok(Some(state::Stream::default()))
    }

    /// Transition the stream state based on receiving headers
    fn recv_headers(&mut self, state: &mut state::Stream) -> Result<(), ConnectionError> {
        state.remote_open(self.config.init_remote_window_sz)
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
