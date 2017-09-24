use super::*;
use super::store::Resolve;
use {client, proto, server};
use codec::{RecvError, SendError, UserError};
use frame::Reason;
use proto::*;

use http::HeaderMap;

use std::io;
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub(crate) struct Streams<B, P>
where
    P: Peer,
{
    inner: Arc<Mutex<Inner<B, P>>>,
}

/// Reference to the stream state
#[derive(Debug)]
pub(crate) struct StreamRef<B, P>
where
    P: Peer,
{
    inner: Arc<Mutex<Inner<B, P>>>,
    key: store::Key,
}

/// Fields needed to manage state related to managing the set of streams. This
/// is mostly split out to make ownership happy.
///
/// TODO: better name
#[derive(Debug)]
struct Inner<B, P>
where
    P: Peer,
{
    /// Tracks send & recv stream concurrency.
    counts: Counts<P>,
    actions: Actions<B, P>,
    store: Store<B, P>,
}

#[derive(Debug)]
struct Actions<B, P>
where
    P: Peer,
{
    /// Manages state transitions initiated by receiving frames
    recv: Recv<B, P>,

    /// Manages state transitions initiated by sending frames
    send: Send<B, P>,

    /// Task that calls `poll_complete`.
    task: Option<task::Task>,
}

impl<B, P> Streams<B, P>
where
    B: Buf,
    P: Peer,
{
    pub fn new(config: Config) -> Self {
        Streams {
            inner: Arc::new(Mutex::new(Inner {
                counts: Counts::new(&config),
                actions: Actions {
                    recv: Recv::new(&config),
                    send: Send::new(&config),
                    task: None,
                },
                store: Store::new(),
            })),
        }
    }

    /// Process inbound headers
    pub fn recv_headers(&mut self, frame: frame::Headers) -> Result<(), RecvError> {
        let id = frame.stream_id();
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let key = match me.store.find_entry(id) {
            Entry::Occupied(e) => e.key(),
            Entry::Vacant(e) => match me.actions.recv.open(id, &mut me.counts)? {
                Some(stream_id) => {
                    let stream = Stream::new(
                        stream_id,
                        me.actions.send.init_window_sz(),
                        me.actions.recv.init_window_sz(),
                    );

                    e.insert(stream)
                },
                None => return Ok(()),
            },
        };

        let stream = me.store.resolve(key);
        let actions = &mut me.actions;

        me.counts.transition(stream, |counts, stream| {
            trace!(
                "recv_headers; stream={:?}; state={:?}",
                stream.id,
                stream.state
            );

            let res = if stream.state.is_recv_headers() {
                actions.recv.recv_headers(frame, stream, counts)
            } else {
                if !frame.is_end_stream() {
                    // TODO: Is this the right error
                    return Err(RecvError::Connection(ProtocolError));
                }

                actions.recv.recv_trailers(frame, stream)
            };

            actions.reset_on_recv_stream_err(stream, res)
        })
    }

    pub fn recv_data(&mut self, frame: frame::Data) -> Result<(), RecvError> {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let id = frame.stream_id();

        let stream = match me.store.find_mut(&id) {
            Some(stream) => stream,
            None => return Err(RecvError::Connection(ProtocolError)),
        };

        let actions = &mut me.actions;

        me.counts.transition(stream, |_, stream| {
            let res = actions.recv.recv_data(frame, stream);
            actions.reset_on_recv_stream_err(stream, res)
        })
    }

    pub fn recv_reset(&mut self, frame: frame::Reset) -> Result<(), RecvError> {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let id = frame.stream_id();

        if id.is_zero() {
            return Err(RecvError::Connection(ProtocolError));
        }

        let stream = match me.store.find_mut(&id) {
            Some(stream) => stream,
            None => {
                // TODO: Are there other error cases?
                me.actions
                    .ensure_not_idle(id)
                    .map_err(RecvError::Connection)?;

                return Ok(());
            },
        };

        let actions = &mut me.actions;

        me.counts.transition(stream, |_, stream| {
            actions.recv.recv_reset(frame, stream)?;
            assert!(stream.state.is_closed());
            Ok(())
        })
    }

    /// Handle a received error and return the ID of the last processed stream.
    pub fn recv_err(&mut self, err: &proto::Error) -> StreamId {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let actions = &mut me.actions;
        let counts = &mut me.counts;

        let last_processed_id = actions.recv.last_processed_id();

        me.store
            .for_each(|stream| {
                counts.transition(stream, |_, stream| {
                    actions.recv.recv_err(err, &mut *stream);
                    Ok::<_, ()>(())
                })
            })
            .unwrap();

        last_processed_id
    }

    pub fn recv_window_update(&mut self, frame: frame::WindowUpdate) -> Result<(), RecvError> {
        let id = frame.stream_id();
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        if id.is_zero() {
            me.actions
                .send
                .recv_connection_window_update(frame, &mut me.store)
                .map_err(RecvError::Connection)?;
        } else {
            // The remote may send window updates for streams that the local now
            // considers closed. It's ok...
            if let Some(mut stream) = me.store.find_mut(&id) {
                // This result is ignored as there is nothing to do when there
                // is an error. The stream is reset by the function on error and
                // the error is informational.
                let _ = me.actions.send.recv_stream_window_update(
                    frame.size_increment(),
                    &mut stream,
                    &mut me.actions.task,
                );
            } else {
                me.actions
                    .recv
                    .ensure_not_idle(id)
                    .map_err(RecvError::Connection)?;
            }
        }

        Ok(())
    }

    pub fn recv_push_promise(&mut self, frame: frame::PushPromise) -> Result<(), RecvError> {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let id = frame.stream_id();

        let stream = match me.store.find_mut(&id) {
            Some(stream) => stream.key(),
            None => return Err(RecvError::Connection(ProtocolError)),
        };

        me.actions
            .recv
            .recv_push_promise(frame, &me.actions.send, stream, &mut me.store)
    }

    pub fn next_incoming(&mut self) -> Option<StreamRef<B, P>> {
        let key = {
            let mut me = self.inner.lock().unwrap();
            let me = &mut *me;

            match me.actions.recv.next_incoming(&mut me.store) {
                Some(key) => {
                    // Increment the ref count
                    me.store.resolve(key).ref_inc();

                    // Return the key
                    Some(key)
                },
                None => None,
            }
        };

        key.map(|key| {
            StreamRef {
                inner: self.inner.clone(),
                key,
            }
        })
    }

    pub fn send_pending_refusal<T>(
        &mut self,
        dst: &mut Codec<T, Prioritized<B>>,
    ) -> Poll<(), io::Error>
    where
        T: AsyncWrite,
    {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;
        me.actions.recv.send_pending_refusal(dst)
    }

    pub fn poll_complete<T>(&mut self, dst: &mut Codec<T, Prioritized<B>>) -> Poll<(), io::Error>
    where
        T: AsyncWrite,
    {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        // Send WINDOW_UPDATE frames first
        //
        // TODO: It would probably be better to interleave updates w/ data
        // frames.
        try_ready!(me.actions.recv.poll_complete(&mut me.store, dst));

        // Send any other pending frames
        try_ready!(me.actions.send.poll_complete(
            &mut me.store,
            &mut me.counts,
            dst
        ));

        // Nothing else to do, track the task
        me.actions.task = Some(task::current());

        Ok(().into())
    }

    pub fn apply_remote_settings(&mut self, frame: &frame::Settings) -> Result<(), RecvError> {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        me.counts.apply_remote_settings(frame);

        me.actions
            .send
            .apply_remote_settings(frame, &mut me.store, &mut me.actions.task)
    }

    pub fn send_request(
        &mut self,
        request: Request<()>,
        end_of_stream: bool,
    ) -> Result<StreamRef<B, P>, SendError> {
        use super::stream::ContentLength;
        use http::Method;

        // TODO: There is a hazard with assigning a stream ID before the
        // prioritize layer. If prioritization reorders new streams, this
        // implicitly closes the earlier stream IDs.
        //
        // See: carllerche/h2#11
        let key = {
            let mut me = self.inner.lock().unwrap();
            let me = &mut *me;

            // Initialize a new stream. This fails if the connection is at capacity.
            let stream_id = me.actions.send.open(&mut me.counts)?;

            let mut stream = Stream::new(
                stream_id,
                me.actions.send.init_window_sz(),
                me.actions.recv.init_window_sz(),
            );

            if *request.method() == Method::HEAD {
                stream.content_length = ContentLength::Head;
            }

            // Convert the message
            let headers = client::Peer::convert_send_message(stream_id, request, end_of_stream);

            let mut stream = me.store.insert(stream.id, stream);

            me.actions
                .send
                .send_headers(headers, &mut stream, &mut me.actions.task)?;

            // Given that the stream has been initialized, it should not be in the
            // closed state.
            debug_assert!(!stream.state.is_closed());

            // Increment the stream ref count as we will be returning a handle.
            stream.ref_inc();

            stream.key()
        };

        Ok(StreamRef {
            inner: self.inner.clone(),
            key: key,
        })
    }

    pub fn send_reset(&mut self, id: StreamId, reason: Reason) {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let key = match me.store.find_entry(id) {
            Entry::Occupied(e) => e.key(),
            Entry::Vacant(e) => match me.actions.recv.open(id, &mut me.counts) {
                Ok(Some(stream_id)) => {
                    let stream = Stream::new(stream_id, 0, 0);

                    e.insert(stream)
                },
                _ => return,
            },
        };

        let stream = me.store.resolve(key);
        let actions = &mut me.actions;

        me.counts.transition(stream, |_, stream| {
            actions.send.send_reset(reason, stream, &mut actions.task)
        })
    }
}

impl<B> Streams<B, client::Peer>
where
    B: Buf,
{
    pub fn poll_send_request_ready(&mut self) -> Poll<(), ::Error> {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        me.actions.send.ensure_next_stream_id()?;

        Ok(me.counts.poll_open_ready())
    }
}

#[cfg(feature = "unstable")]
impl<B, P> Streams<B, P>
where
    B: Buf,
    P: Peer,
{
    pub fn num_active_streams(&self) -> usize {
        let me = self.inner.lock().unwrap();
        me.store.num_active_streams()
    }

    pub fn num_wired_streams(&self) -> usize {
        let me = self.inner.lock().unwrap();
        me.store.num_wired_streams()
    }
}

// ===== impl StreamRef =====

impl<B, P> StreamRef<B, P>
where
    B: Buf,
    P: Peer,
{
    pub fn send_data(&mut self, data: B, end_stream: bool) -> Result<(), UserError> {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let stream = me.store.resolve(self.key);
        let actions = &mut me.actions;

        me.counts.transition(stream, |_, stream| {
            // Create the data frame
            let mut frame = frame::Data::new(stream.id, data);
            frame.set_end_stream(end_stream);

            // Send the data frame
            actions.send.send_data(frame, stream, &mut actions.task)
        })
    }

    pub fn send_trailers(&mut self, trailers: HeaderMap) -> Result<(), UserError> {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let stream = me.store.resolve(self.key);
        let actions = &mut me.actions;

        me.counts.transition(stream, |_, stream| {
            // Create the trailers frame
            let frame = frame::Headers::trailers(stream.id, trailers);

            // Send the trailers frame
            actions.send.send_trailers(frame, stream, &mut actions.task)
        })
    }

    pub fn send_reset(&mut self, reason: Reason) {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let stream = me.store.resolve(self.key);
        let actions = &mut me.actions;

        me.counts.transition(stream, |_, stream| {
            actions.send.send_reset(reason, stream, &mut actions.task)
        })
    }

    pub fn send_response(
        &mut self,
        response: Response<()>,
        end_of_stream: bool,
    ) -> Result<(), UserError> {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let stream = me.store.resolve(self.key);
        let actions = &mut me.actions;

        me.counts.transition(stream, |_, stream| {
            let frame = server::Peer::convert_send_message(stream.id, response, end_of_stream);

            actions.send.send_headers(frame, stream, &mut actions.task)
        })
    }

    pub fn body_is_empty(&self) -> bool {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let stream = me.store.resolve(self.key);

        me.actions.recv.body_is_empty(&stream)
    }

    pub fn poll_data(&mut self) -> Poll<Option<Bytes>, proto::Error> {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let mut stream = me.store.resolve(self.key);

        me.actions.recv.poll_data(&mut stream)
    }

    pub fn poll_trailers(&mut self) -> Poll<Option<HeaderMap>, proto::Error> {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let mut stream = me.store.resolve(self.key);

        me.actions.recv.poll_trailers(&mut stream)
    }

    /// Releases recv capacity back to the peer. This may result in sending
    /// WINDOW_UPDATE frames on both the stream and connection.
    pub fn release_capacity(&mut self, capacity: WindowSize) -> Result<(), UserError> {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let mut stream = me.store.resolve(self.key);

        me.actions
            .recv
            .release_capacity(capacity, &mut stream, &mut me.actions.task)
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
    pub fn poll_capacity(&mut self) -> Poll<Option<WindowSize>, UserError> {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let mut stream = me.store.resolve(self.key);

        me.actions.send.poll_capacity(&mut stream)
    }
}

impl<B> StreamRef<B, server::Peer>
where
    B: Buf,
{
    /// Called by the server after the stream is accepted. Given that clients
    /// initialize streams by sending HEADERS, the request will always be
    /// available.
    ///
    /// # Panics
    ///
    /// This function panics if the request isn't present.
    pub fn take_request(&self) -> Request<()> {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let mut stream = me.store.resolve(self.key);
        me.actions.recv.take_request(&mut stream)
    }
}

impl<B> StreamRef<B, client::Peer>
where
    B: Buf,
{
    pub fn poll_response(&mut self) -> Poll<Response<()>, proto::Error> {
        let mut me = self.inner.lock().unwrap();
        let me = &mut *me;

        let mut stream = me.store.resolve(self.key);

        me.actions.recv.poll_response(&mut stream)
    }
}

impl<B, P> Clone for StreamRef<B, P>
where
    P: Peer,
{
    fn clone(&self) -> Self {
        // Increment the ref count
        self.inner.lock().unwrap().store.resolve(self.key).ref_inc();

        StreamRef {
            inner: self.inner.clone(),
            key: self.key.clone(),
        }
    }
}

impl<B, P> Drop for StreamRef<B, P>
where
    P: Peer,
{
    fn drop(&mut self) {
        let mut me = self.inner.lock().unwrap();

        let me = &mut *me;

        let id = {
            let mut stream = me.store.resolve(self.key);
            stream.ref_dec();

            if !stream.is_released() {
                return;
            }

            stream.remove()
        };

        debug_assert!(!me.store.contains_id(&id));
    }
}

// ===== impl Actions =====

impl<B, P> Actions<B, P>
where
    B: Buf,
    P: Peer,
{
    fn reset_on_recv_stream_err(
        &mut self,
        stream: &mut store::Ptr<B, P>,
        res: Result<(), RecvError>,
    ) -> Result<(), RecvError> {
        if let Err(RecvError::Stream {
            reason, ..
        }) = res
        {
            // Reset the stream.
            self.send.send_reset(reason, stream, &mut self.task);
            Ok(())
        } else {
            res
        }
    }

    fn ensure_not_idle(&mut self, id: StreamId) -> Result<(), Reason> {
        if P::is_local_init(id) {
            self.send.ensure_not_idle(id)
        } else {
            self.recv.ensure_not_idle(id)
        }
    }
}
