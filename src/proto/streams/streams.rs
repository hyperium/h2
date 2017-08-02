use proto::*;
use super::*;

use std::sync::{Arc, Mutex};

// TODO: All the VecDeques should become linked lists using the State
// values.
#[derive(Debug)]
pub struct Streams<P> {
    inner: Arc<Mutex<Inner<P>>>,
}

/// Fields needed to manage state related to managing the set of streams. This
/// is mostly split out to make ownership happy.
///
/// TODO: better name
#[derive(Debug)]
struct Inner<P> {
    actions: Actions<P>,
    store: Store,
}

#[derive(Debug)]
struct Actions<P> {
    /// Manages state transitions initiated by receiving frames
    recv: Recv<P>,

    /// Manages state transitions initiated by sending frames
    send: Send<P>,
}

impl<P: Peer> Streams<P> {
    pub fn new(config: Config) -> Self {
        Streams {
            inner: Arc::new(Mutex::new(Inner {
                actions: Actions {
                    recv: Recv::new(&config),
                    send: Send::new(&config),
                },
                store: Store::new(),
            })),
        }
    }

    pub fn recv_headers(&mut self, frame: frame::Headers)
        -> Result<Option<frame::Headers>, ConnectionError>
    {
        let id = frame.stream_id();
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let state = match me.store.entry(id) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                // Trailers cannot open a stream. Trailers are header frames
                // that do not contain pseudo headers. Requests MUST contain a
                // method and responses MUST contain a status. If they do not,t
                // hey are considered to be malformed.
                if frame.is_trailers() {
                    return Err(ProtocolError.into());
                }

                match try!(me.actions.recv.open(id)) {
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

            try!(me.actions.recv.recv_eos(state));
        } else {
            try!(me.actions.recv.recv_headers(state, frame.is_end_stream()));
        }

        if state.is_closed() {
            me.actions.dec_num_streams(id);
        }

        Ok(Some(frame))
    }

    pub fn recv_data(&mut self, frame: &frame::Data)
        -> Result<(), ConnectionError>
    {
        let id = frame.stream_id();
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let state = match me.store.get_mut(&id) {
            Some(state) => state,
            None => return Err(ProtocolError.into()),
        };

        // Ensure there's enough capacity on the connection before acting on the
        // stream.
        try!(me.actions.recv.recv_data(frame, state));

        if state.is_closed() {
            me.actions.dec_num_streams(id);
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
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        if id.is_zero() {
            try!(me.actions.send.recv_connection_window_update(frame));
        } else {
            // The remote may send window updates for streams that the local now
            // considers closed. It's ok...
            if let Some(state) = me.store.get_mut(&id) {
                try!(me.actions.send.recv_stream_window_update(frame, state));
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
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        trace!("send_headers; id={:?}", id);

        let state = match me.store.entry(id) {
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

                let state = try!(me.actions.send.open(id));
                e.insert(state)
            }
        };

        if frame.is_trailers() {
            try!(me.actions.send.send_eos(state));
        } else {
            try!(me.actions.send.send_headers(state, frame.is_end_stream()));
        }

        if state.is_closed() {
            me.actions.dec_num_streams(id);
        }

        Ok(())
    }

    pub fn send_data<B: Buf>(&mut self, frame: &frame::Data<B>)
        -> Result<(), ConnectionError>
    {
        let id = frame.stream_id();
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let state = match me.store.get_mut(&id) {
            Some(state) => state,
            None => return Err(UnexpectedFrameType.into()),
        };

        // Ensure there's enough capacity on the connection before acting on the
        // stream.
        try!(me.actions.send.send_data(frame, state));

        if state.is_closed() {
            me.actions.dec_num_streams(id);
        }

        Ok(())
    }

    pub fn poll_window_update(&mut self)
        -> Poll<WindowUpdate, ConnectionError>
    {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;
        me.actions.send.poll_window_update(&mut me.store)
    }

    pub fn expand_window(&mut self, id: StreamId, sz: WindowSize)
        -> Result<(), ConnectionError>
    {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        if id.is_zero() {
            try!(me.actions.recv.expand_connection_window(sz));
        } else {
            if let Some(state) = me.store.get_mut(&id) {
                try!(me.actions.recv.expand_stream_window(id, sz, state));
            }
        }

        Ok(())
    }

    pub fn send_pending_refusal<T, B>(&mut self, dst: &mut Codec<T, B>)
        -> Poll<(), ConnectionError>
        where T: AsyncWrite,
              B: Buf,
    {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;
        me.actions.recv.send_pending_refusal(dst)
    }

    pub fn send_pending_window_updates<T, B>(&mut self, dst: &mut Codec<T, B>)
        -> Poll<(), ConnectionError>
        where T: AsyncWrite,
              B: Buf,
    {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;
        try_ready!(me.actions.recv.send_connection_window_update(dst));
        try_ready!(me.actions.recv.send_stream_window_update(&mut me.store, dst));

        Ok(().into())
    }
}

impl<P: Peer> Actions<P> {
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
