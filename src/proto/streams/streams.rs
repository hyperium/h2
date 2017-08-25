use {client, server, HeaderMap};
use proto::*;
use super::*;
use super::store::Resolve;

use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub(crate) struct Streams<B> {
    inner: Arc<Mutex<Inner<B>>>,
}

/// Reference to the stream state
#[derive(Debug)]
pub(crate) struct StreamRef<B> {
    inner: Arc<Mutex<Inner<B>>>,
    key: store::Key,
}

/// Fields needed to manage state related to managing the set of streams. This
/// is mostly split out to make ownership happy.
///
/// TODO: better name
#[derive(Debug)]
struct Inner<B> {
    actions: Actions<B>,
    store: Store<B>,
}

#[derive(Debug)]
struct Actions<B> {
    /// Manages state transitions initiated by receiving frames
    recv: Recv<B>,

    /// Manages state transitions initiated by sending frames
    send: Send<B>,

    /// Task that calls `poll_complete`.
    task: Option<task::Task>,
}

impl<B> Streams<B>
    where B: Buf,
{
    pub fn new<P: Peer>(config: Config) -> Self {
        Streams {
            inner: Arc::new(Mutex::new(Inner {
                actions: Actions {
                    recv: Recv::new::<P>(&config),
                    send: Send::new::<P>(&config),
                    task: None,
                },
                store: Store::new(),
            })),
        }
    }

    /// Process inbound headers
    pub fn recv_headers<P: Peer>(&mut self, frame: frame::Headers)
        -> Result<(), ConnectionError>
    {
        let id = frame.stream_id();
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let key = match me.store.find_entry(id) {
            Entry::Occupied(e) => e.key(),
            Entry::Vacant(e) => {
                // Trailers cannot open a stream. Trailers are header frames
                // that do not contain pseudo headers. Requests MUST contain a
                // method and responses MUST contain a status. If they do not,t
                // hey are considered to be malformed.
                if frame.is_trailers() {
                    return Err(ProtocolError.into());
                }

                match try!(me.actions.recv.open::<P>(id)) {
                    Some(stream_id) => {
                        let stream = Stream::new(
                            stream_id,
                            me.actions.send.init_window_sz(),
                            me.actions.recv.init_window_sz());

                        e.insert(stream)
                    }
                    None => return Ok(()),
                }
            }
        };

        let stream = me.store.resolve(key);

        me.actions.transition::<P, _, _>(stream, |actions, stream| {
            if frame.is_trailers() {
                if !frame.is_end_stream() {
                    // TODO: Is this the right error
                    return Err(ProtocolError.into());
                }

                actions.recv.recv_trailers::<P>(frame, stream)
            } else {
                actions.recv.recv_headers::<P>(frame, stream)
            }
        })
    }

    pub fn recv_data<P: Peer>(&mut self, frame: frame::Data)
        -> Result<(), ConnectionError>
    {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let id = frame.stream_id();

        let stream = match me.store.find_mut(&id) {
            Some(stream) => stream,
            None => return Err(ProtocolError.into()),
        };

        me.actions.transition::<P, _, _>(stream, |actions, stream| {
            actions.recv.recv_data(frame, stream)
        })
    }

    pub fn recv_reset<P: Peer>(&mut self, frame: frame::Reset)
        -> Result<(), ConnectionError>
    {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let id = frame.stream_id();

        if id.is_zero() {
            return Err(ProtocolError.into());
        }

        let stream = match me.store.find_mut(&id) {
            Some(stream) => stream,
            None => {
                // TODO: Are there other error cases?
                me.actions.ensure_not_idle::<P>(id)?;
                return Ok(());
            }
        };

        me.actions.transition::<P, _, _>(stream, |actions, stream| {
            actions.recv.recv_reset(frame, stream)?;
            assert!(stream.state.is_closed());
            Ok(())
        })
    }

    /// Handle a received error and return the ID of the last processed stream.
    pub fn recv_err(&mut self, err: &ConnectionError) -> StreamId {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let actions = &mut me.actions;
        let last_processed_id = actions.recv.last_processed_id();

        me.store.for_each(|mut stream| {
            actions.recv.recv_err(err, &mut *stream);
            Ok(())
        }).ok().expect("unexpected error processing error");

        last_processed_id
    }

    pub fn recv_window_update(&mut self, frame: frame::WindowUpdate)
        -> Result<(), ConnectionError>
    {
        let id = frame.stream_id();
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        if id.is_zero() {
            me.actions.send.recv_connection_window_update(
                frame, &mut me.store)?;
        } else {
            // The remote may send window updates for streams that the local now
            // considers closed. It's ok...
            if let Some(mut stream) = me.store.find_mut(&id) {
                me.actions.send.recv_stream_window_update(
                    frame.size_increment(), &mut stream, &mut me.actions.task)?;
            } else {
                me.actions.recv.ensure_not_idle(id)?;
            }
        }

        Ok(())
    }

    pub fn recv_push_promise<P: Peer>(&mut self, frame: frame::PushPromise)
        -> Result<(), ConnectionError>
    {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let id = frame.stream_id();

        let stream = match me.store.find_mut(&id) {
            Some(stream) => stream.key(),
            None => return Err(ProtocolError.into()),
        };

        me.actions.recv.recv_push_promise::<P>(
            frame, &me.actions.send, stream, &mut me.store)
    }

    pub fn next_incoming(&mut self) -> Option<StreamRef<B>> {
        let key = {
            let mut me = self.inner.lock().unwrap();
            let me = &mut *me;

            me.actions.recv.next_incoming(&mut me.store)
        };

        key.map(|key| {
            StreamRef {
                inner: self.inner.clone(),
                key,
            }
        })
    }

    pub fn send_pending_refusal<T>(&mut self, dst: &mut Codec<T, Prioritized<B>>)
        -> Poll<(), ConnectionError>
        where T: AsyncWrite,
    {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;
        me.actions.recv.send_pending_refusal(dst)
    }

    pub fn poll_complete<T>(&mut self, dst: &mut Codec<T, Prioritized<B>>)
        -> Poll<(), ConnectionError>
        where T: AsyncWrite,
    {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        // Send WINDOW_UPDATE frames first
        //
        // TODO: It would probably be better to interleave updates w/ data
        // frames.
        try_ready!(me.actions.recv.poll_complete(&mut me.store, dst));

        // Send any other pending frames
        try_ready!(me.actions.send.poll_complete(&mut me.store, dst));

        // Nothing else to do, track the task
        me.actions.task = Some(task::current());

        Ok(().into())
    }

    pub fn apply_remote_settings(&mut self, frame: &frame::Settings)
        -> Result<(), ConnectionError>
    {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        me.actions.send.apply_remote_settings(
            frame, &mut me.store, &mut me.actions.task)
    }

    pub fn poll_send_request_ready(&mut self) -> Poll<(), ConnectionError> {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        me.actions.send.poll_open_ready::<client::Peer>()
    }

    pub fn send_request(&mut self, request: Request<()>, end_of_stream: bool)
        -> Result<StreamRef<B>, ConnectionError>
    {
        // TODO: There is a hazard with assigning a stream ID before the
        // prioritize layer. If prioritization reorders new streams, this
        // implicitly closes the earlier stream IDs.
        //
        // See: carllerche/h2#11
        let key = {
            let mut me = self.inner.lock().unwrap();
            let me = &mut *me;

            // Initialize a new stream. This fails if the connection is at capacity.
            let stream_id = me.actions.send.open::<client::Peer>()?;

            let stream = Stream::new(
                stream_id,
                me.actions.send.init_window_sz(),
                me.actions.recv.init_window_sz());

            // Convert the message
            let headers = client::Peer::convert_send_message(
                stream_id, request, end_of_stream);

            let mut stream = me.store.insert(stream.id, stream);

            me.actions.send.send_headers(
                headers, &mut stream, &mut me.actions.task)?;

            // Given that the stream has been initialized, it should not be in the
            // closed state.
            debug_assert!(!stream.state.is_closed());

            stream.key()
        };

        Ok(StreamRef {
            inner: self.inner.clone(),
            key: key,
        })
    }

    pub fn send_reset<P: Peer>(&mut self, id: StreamId, reason: Reason) {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let key = match me.store.find_entry(id) {
            Entry::Occupied(e) => e.key(),
            Entry::Vacant(e) => {
                match me.actions.recv.open::<P>(id) {
                    Ok(Some(stream_id)) => {
                        let stream = Stream::new(
                            stream_id, 0, 0);

                        e.insert(stream)
                    }
                    _ => return,
                }
            }
        };


        let stream = me.store.resolve(key);

        me.actions.transition::<P, _, _>(stream, move |actions, stream| {
            actions.send.send_reset(reason, stream, &mut actions.task)
        })
    }
}

// ===== impl StreamRef =====

impl<B> StreamRef<B>
    where B: Buf,
{
    pub fn send_data<P: Peer>(&mut self, data: B, end_of_stream: bool)
        -> Result<(), ConnectionError>
    {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let stream = me.store.resolve(self.key);

        // Create the data frame
        let frame = frame::Data::from_buf(stream.id, data, end_of_stream);

        me.actions.transition::<P, _, _>(stream, |actions, stream| {
            // Send the data frame
            actions.send.send_data(frame, stream, &mut actions.task)
        })
    }

    pub fn send_trailers<P: Peer>(&mut self, trailers: HeaderMap) -> Result<(), ConnectionError>
    {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let stream = me.store.resolve(self.key);

        // Create the trailers frame
        let frame = frame::Headers::trailers(stream.id, trailers);

        me.actions.transition::<P, _, _>(stream, |actions, stream| {
            // Send the trailers frame
            actions.send.send_trailers(frame, stream, &mut actions.task)
        })
    }

    /// Called by the server after the stream is accepted. Given that clients
    /// initialize streams by sending HEADERS, the request will always be
    /// available.
    ///
    /// # Panics
    ///
    /// This function panics if the request isn't present.
    pub fn take_request(&self) -> Result<Request<()>, ConnectionError> {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let mut stream = me.store.resolve(self.key);
        me.actions.recv.take_request(&mut stream)
    }

    pub fn send_reset<P: Peer>(&mut self, reason: Reason) {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let stream = me.store.resolve(self.key);
        me.actions.transition::<P, _, _>(stream, move |actions, stream| {
            actions.send.send_reset(reason, stream, &mut actions.task)
        })
    }

    pub fn send_response(&mut self, response: Response<()>, end_of_stream: bool)
        -> Result<(), ConnectionError>
    {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let stream = me.store.resolve(self.key);

        let frame = server::Peer::convert_send_message(
            stream.id, response, end_of_stream);

        me.actions.transition::<server::Peer, _, _>(stream, |actions, stream| {
            actions.send.send_headers(frame, stream, &mut actions.task)
        })
    }

    pub fn body_is_empty(&self) -> bool {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let stream = me.store.resolve(self.key);

        me.actions.recv.body_is_empty(&stream)
    }

    pub fn poll_response(&mut self) -> Poll<Response<()>, ConnectionError> {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let mut stream = me.store.resolve(self.key);

        me.actions.recv.poll_response(&mut stream)
    }

    pub fn poll_data(&mut self) -> Poll<Option<Bytes>, ConnectionError> {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let mut stream = me.store.resolve(self.key);

        me.actions.recv.poll_data(&mut stream)
    }

    pub fn poll_trailers(&mut self) -> Poll<Option<HeaderMap>, ConnectionError> {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let mut stream = me.store.resolve(self.key);

        me.actions.recv.poll_trailers(&mut stream)
    }

    /// Releases recv capacity back to the peer. This will result in sending
    /// WINDOW_UPDATE frames on both the stream and connection.
    pub fn release_capacity(&mut self, capacity: WindowSize)
        -> Result<(), ConnectionError>
    {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let mut stream = me.store.resolve(self.key);

        me.actions.recv.release_capacity(
            capacity, &mut stream, &mut me.actions.task)
    }

    /// Request capacity to send data
    pub fn reserve_capacity(&mut self, capacity: WindowSize) {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let mut stream = me.store.resolve(self.key);

        me.actions.send.reserve_capacity(capacity, &mut stream)
    }

    /// Returns the stream's current send capacity.
    pub fn capacity(&self) -> WindowSize {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let mut stream = me.store.resolve(self.key);

        me.actions.send.capacity(&mut stream)
    }

    /// Request to be notified when the stream's capacity increases
    pub fn poll_capacity(&mut self) -> Poll<Option<WindowSize>, ConnectionError> {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let mut stream = me.store.resolve(self.key);

        me.actions.send.poll_capacity(&mut stream)
    }
}

impl<B> Clone for StreamRef<B> {
    fn clone(&self) -> Self {
        StreamRef {
            inner: self.inner.clone(),
            key: self.key.clone(),
        }
    }
}

// ===== impl Actions =====

impl<B> Actions<B>
    where B: Buf,
{
    fn ensure_not_idle<P: Peer>(&mut self, id: StreamId)
        -> Result<(), ConnectionError>
    {
        if self.is_local_init::<P>(id) {
            self.send.ensure_not_idle(id)
        } else {
            self.recv.ensure_not_idle(id)
        }
    }

    fn dec_num_streams<P: Peer>(&mut self, id: StreamId) {
        if self.is_local_init::<P>(id) {
            self.send.dec_num_streams();
        } else {
            self.recv.dec_num_streams();
        }
    }

    fn is_local_init<P: Peer>(&self, id: StreamId) -> bool {
        assert!(!id.is_zero());
        P::is_server() == id.is_server_initiated()
    }

    fn transition<P, F, U>(&mut self, mut stream: store::Ptr<B>, f: F) -> U
        where F: FnOnce(&mut Self, &mut store::Ptr<B>) -> U,
              P: Peer,
    {
        let is_counted = stream.state.is_counted();

        let ret = f(self, &mut stream);

        if is_counted && stream.state.is_closed() {
            self.dec_num_streams::<P>(stream.id);
        }

        ret
    }
}
