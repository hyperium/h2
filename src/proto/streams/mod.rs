mod recv;
mod send;

use self::recv::Recv;
use self::send::Send;

use {frame, Peer, StreamId, ConnectionError};
use proto::*;
use error::Reason::*;
use error::User::*;

use ordermap::{OrderMap, Entry};

// TODO: All the VecDeques should become linked lists using the state::Stream
// values.
#[derive(Debug)]
pub struct Streams<P> {
    /// State related to managing the set of streams.
    inner: Inner<P>,

    /// Streams
    streams: StreamMap,
}

type StreamMap = OrderMap<StreamId, state::Stream>;

/// Fields needed to manage state related to managing the set of streams. This
/// is mostly split out to make ownership happy.
///
/// TODO: better name
#[derive(Debug)]
struct Inner<P> {
    /// Manages state transitions initiated by receiving frames
    recv: Recv<P>,

    /// Manages state transitions initiated by sending frames
    send: Send<P>,
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

impl<P: Peer> Streams<P> {
    pub fn new(config: Config) -> Self {
        Streams {
            inner: Inner {
                recv: Recv::new(&config),
                send: Send::new(&config),
            },
            streams: OrderMap::default(),
        }
    }

    pub fn recv_headers(&mut self, frame: frame::Headers)
        -> Result<Option<frame::Headers>, ConnectionError>
    {
        let id = frame.stream_id();

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

                match try!(self.inner.recv.open(id)) {
                    Some(state) => e.insert(state),
                    None => return Ok(None),
                }
            }
        };

        if frame.is_trailers() {
            if !frame.is_end_stream() {
                // TODO: What error should this return?
                unimplemented!();
            }

            try!(self.inner.recv.recv_eos(state));
        } else {
            try!(self.inner.recv.recv_headers(state, frame.is_end_stream()));
        }

        if state.is_closed() {
            self.inner.dec_num_streams(id);
        }

        Ok(Some(frame))
    }

    pub fn recv_data(&mut self, frame: &frame::Data)
        -> Result<(), ConnectionError>
    {
        let id = frame.stream_id();

        let state = match self.streams.get_mut(&id) {
            Some(state) => state,
            None => return Err(ProtocolError.into()),
        };

        // Ensure there's enough capacity on the connection before acting on the
        // stream.
        try!(self.inner.recv.recv_data(frame, state));

        if state.is_closed() {
            self.inner.dec_num_streams(id);
        }

        Ok(())
    }

    pub fn recv_reset(&mut self, _frame: &frame::Reset)
        -> Result<(), ConnectionError>
    {
        unimplemented!();
    }

    pub fn recv_window_update(&mut self, frame: frame::WindowUpdate)
        -> Result<(), ConnectionError> {
        let id = frame.stream_id();

        if id.is_zero() {
            try!(self.inner.send.recv_connection_window_update(frame));
        } else {
            // The remote may send window updates for streams that the local now
            // considers closed. It's ok...
            if let Some(state) = self.streams.get_mut(&id) {
                try!(self.inner.send.recv_stream_window_update(frame, state));
            }
        }

        Ok(())
    }

    pub fn recv_push_promise(&mut self, _frame: frame::PushPromise)
        -> Result<(), ConnectionError>
    {
        unimplemented!();
    }

    pub fn send_headers(&mut self, frame: &frame::Headers)
        -> Result<(), ConnectionError>
    {
        let id = frame.stream_id();

        trace!("send_headers; id={:?}", id);

        let state = match self.streams.entry(id) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                // Trailers cannot open a stream. Trailers are header frames
                // that do not contain pseudo headers. Requests MUST contain a
                // method and responses MUST contain a status. If they do not,t
                // hey are considered to be malformed.
                if frame.is_trailers() {
                    // TODO: Should this be a different error?
                    return Err(UnexpectedFrameType.into());
                }

                let state = try!(self.inner.send.open(id));
                e.insert(state)
            }
        };

        if frame.is_trailers() {
            try!(self.inner.send.send_eos(state));
        } else {
            try!(self.inner.send.send_headers(state, frame.is_end_stream()));
        }

        if state.is_closed() {
            self.inner.dec_num_streams(id);
        }

        Ok(())
    }

    pub fn send_data<B: Buf>(&mut self, frame: &frame::Data<B>)
        -> Result<(), ConnectionError>
    {
        let id = frame.stream_id();

        let state = match self.streams.get_mut(&id) {
            Some(state) => state,
            None => return Err(UnexpectedFrameType.into()),
        };

        // Ensure there's enough capacity on the connection before acting on the
        // stream.
        try!(self.inner.send.send_data(frame, state));

        if state.is_closed() {
            self.inner.dec_num_streams(id);
        }

        Ok(())
    }

    pub fn poll_window_update(&mut self)
        -> Poll<WindowUpdate, ConnectionError>
    {
        self.inner.send.poll_window_update(&mut self.streams)
    }

    pub fn expand_window(&mut self, id: StreamId, sz: WindowSize)
        -> Result<(), ConnectionError>
    {
        if id.is_zero() {
            try!(self.inner.recv.expand_connection_window(sz));
        } else {
            if let Some(state) = self.streams.get_mut(&id) {
                try!(self.inner.recv.expand_stream_window(id, sz, state));
            }
        }

        Ok(())
    }

    pub fn send_pending_refusal<T, B>(&mut self, dst: &mut Codec<T, B>)
        -> Poll<(), ConnectionError>
        where T: AsyncWrite,
              B: Buf,
    {
        self.inner.recv.send_pending_refusal(dst)
    }

    pub fn send_pending_window_updates<T, B>(&mut self, dst: &mut Codec<T, B>)
        -> Poll<(), ConnectionError>
        where T: AsyncWrite,
              B: Buf,
    {
        try_ready!(self.inner.recv.send_connection_window_update(dst));
        try_ready!(self.inner.recv.send_stream_window_update(&mut self.streams, dst));

        Ok(().into())
    }
}

impl<P: Peer> Inner<P> {
    fn dec_num_streams(&mut self, id: StreamId) {
        if self.is_local_init(id) {
            self.send.dec_num_streams();
        } else {
            self.recv.dec_num_streams();
        }
    }

    fn is_local_init(&self, id: StreamId) -> bool {
        assert!(!id.is_zero());
        P::is_server() == id.is_server_initiated()
    }
}
