use {ConnectionError, Peer, StreamId};
use error::Reason::{NoError, ProtocolError};
use proto::*;
use proto::state::StreamState;

use fnv::FnvHasher;
use ordermap::OrderMap;
use std::hash::BuildHasherDefault;
use std::marker::PhantomData;

/// Exposes stream states to "upper" layers of the transport (i.e. from StreamTracker up
/// to Connection).
pub trait ControlStreams {
    /// Determines whether the given stream could theoretically be opened by the local
    /// side of this connection.
    fn local_valid_id(id: StreamId) -> bool;

    /// Determines whether the given stream could theoretically be opened by the remote
    /// side of this connection.
    fn remote_valid_id(id: StreamId) -> bool;

    /// Indicates whether this local endpoint may open streams (with HEADERS).
    ///
    /// Implies that this endpoint is a client.
    fn local_can_open() -> bool;

    /// Indicates whether this remote endpoint may open streams (with HEADERS).
    ///
    /// Implies that this endpoint is a server.
    fn remote_can_open() -> bool {
        !Self::local_can_open()
    }

    /// Creates a new stream in the OPEN state from the local side (i.e. as a Client).
    ///
    /// Must only be called when local_can_open returns true.
    fn local_open(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError>;

    /// Create a new stream in the OPEN state from the remote side (i.e. as a Server).
    ///
    /// Must only be called when remote_can_open returns true.
    fn remote_open(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError>;

    /// Prepare the receive side of a local stream to receive data from the remote.
    ///
    /// Typically called when a client receives a response header.
    fn local_open_recv_half(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError>;

    /// Prepare the send side of a remote stream to receive data from the local endpoint.
    ///
    /// Typically called when a server sends a response header.
    fn remote_open_send_half(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError>;

    // TODO push promise
    // fn local_reserve(&mut self, id: StreamId) -> Result<(), ConnectionError>;
    // fn remote_reserve(&mut self, id: StreamId) -> Result<(), ConnectionError>;

    /// Closes the send half of a stream.
    ///
    /// Fails with a ProtocolError if send half of the stream was not open.
    fn close_send_half(&mut self, id: StreamId) -> Result<(), ConnectionError>;

    /// Closes the recv half of a stream.
    ///
    /// Fails with a ProtocolError if recv half of the stream was not open.
    fn close_recv_half(&mut self, id: StreamId) -> Result<(), ConnectionError>;

    /// Resets the given stream.
    ///
    /// If the stream was already reset, the stored cause is updated.
    fn reset_stream(&mut self, id: StreamId, cause: Reason);

    /// Get the reason the stream was reset, if it was reset.
    fn get_reset(&self, id: StreamId) -> Option<Reason>;

    /// Returns true if the given stream was opened by the local peer and is not yet
    /// closed.
    fn is_local_active(&self, id: StreamId) -> bool;

    /// Returns true if the given stream was opened by the remote peer and is not yet
    /// closed.
    fn is_remote_active(&self, id: StreamId) -> bool;

    /// Returns true if the given stream was opened and is not yet closed.
    fn is_active(&self, id: StreamId) -> bool {
        if Self::local_valid_id(id) {
            self.is_local_active(id)
        } else {
            self.is_remote_active(id)
        }
    }

    /// Returns the number of open streams initiated by the local peer.
    fn local_active_len(&self) -> usize;

    /// Returns the number of open streams initiated by the remote peer.
    fn remote_active_len(&self) -> usize;

    /// Returns true iff the recv half of the given stream is open.
    fn is_recv_open(&mut self, id: StreamId) -> bool;

    /// Returns true iff the send half of the given stream is open.
    fn is_send_open(&mut self, id: StreamId) -> bool;

    /// If the given stream ID is active and able to recv data, get its mutable recv flow
    /// control state.
    fn recv_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState>;

    /// If the given stream ID is active and able to send data, get its mutable send flow
    /// control state.
    fn send_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState>;

    /// Updates the initial window size for the local peer.
    fn update_inital_recv_window_size(&mut self, old_sz: WindowSize, new_sz: WindowSize);

    /// Updates the initial window size for the remote peer.
    fn update_inital_send_window_size(&mut self, old_sz: WindowSize, new_sz: WindowSize);
}

/// Holds the underlying stream state to be accessed by upper layers.
// TODO track reserved streams
// TODO constrain the size of `reset`
#[derive(Debug, Default)]
pub struct StreamStore<T, P> {
    inner: T,

    /// Holds active streams initiated by the local endpoint.
    local_active: OrderMap<StreamId, StreamState, BuildHasherDefault<FnvHasher>>,

    /// Holds active streams initiated by the remote endpoint.
    remote_active: OrderMap<StreamId, StreamState, BuildHasherDefault<FnvHasher>>,

    /// Holds active streams initiated by the remote.
    reset: OrderMap<StreamId, Reason, BuildHasherDefault<FnvHasher>>,

    _phantom: PhantomData<P>,
}

impl<T, P, U> StreamStore<T, P>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          P: Peer,
{
    pub fn new(inner: T) -> StreamStore<T, P> {
        StreamStore {
            inner,
            local_active: OrderMap::default(),
            remote_active: OrderMap::default(),
            reset: OrderMap::default(),
            _phantom: PhantomData,
        }
    }
}

impl<T, P: Peer> StreamStore<T, P> {
    pub fn get_active(&mut self, id: StreamId) -> Option<&StreamState> {
        assert!(!id.is_zero());
        if P::is_valid_local_stream_id(id) {
            self.local_active.get(&id)
        } else {
            self.remote_active.get(&id)
        }
    }

    pub fn get_active_mut(&mut self, id: StreamId) -> Option<&mut StreamState> {
        assert!(!id.is_zero());
        if P::is_valid_local_stream_id(id) {
            self.local_active.get_mut(&id)
        } else {
            self.remote_active.get_mut(&id)
        }
    }

    pub fn remove_active(&mut self, id: StreamId) {
        assert!(!id.is_zero());
        if P::is_valid_local_stream_id(id) {
            self.local_active.remove(&id);
        } else {
            self.remote_active.remove(&id);
        }
    }
}

impl<T, P: Peer> ControlStreams for StreamStore<T, P> {
    fn local_valid_id(id: StreamId) -> bool {
        P::is_valid_local_stream_id(id)
    }

    fn remote_valid_id(id: StreamId) -> bool {
        P::is_valid_remote_stream_id(id)
    }

    fn local_can_open() -> bool {
        P::local_can_open()
    }

    fn local_open(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError> {
        if !Self::local_valid_id(id) || !Self::local_can_open() {
            return Err(ProtocolError.into());
        }
        if self.local_active.contains_key(&id) {
            return Err(ProtocolError.into());
        }

        self.local_active.insert(id, StreamState::new_open_sending(sz));
        Ok(())
    }

    fn remote_open(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError> {
        if !Self::remote_valid_id(id) || !Self::remote_can_open() {
            return Err(ProtocolError.into());
        }
        if self.remote_active.contains_key(&id) {
            return Err(ProtocolError.into());
        }

        self.remote_active.insert(id, StreamState::new_open_recving(sz));
        Ok(())
    }

    fn local_open_recv_half(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError> {
        if !Self::local_valid_id(id) {
            return Err(ProtocolError.into());
        }

        match self.local_active.get_mut(&id) {
            Some(s) => s.open_recv_half(sz).map(|_| {}),
            None => Err(ProtocolError.into()),
        }
    }

    fn remote_open_send_half(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError> {
        if !Self::remote_valid_id(id) {
            return Err(ProtocolError.into());
        }

        match self.remote_active.get_mut(&id) {
            Some(s) => s.open_send_half(sz).map(|_| {}),
            None => Err(ProtocolError.into()),
        }
    }

    fn close_send_half(&mut self, id: StreamId) -> Result<(), ConnectionError> {
        let fully_closed = self.get_active_mut(id)
            .map(|s| s.close_send_half())
            .unwrap_or_else(|| Err(ProtocolError.into()))?;

        if fully_closed {
            self.remove_active(id);
            self.reset.insert(id, NoError);
        }
        Ok(())
    }

    fn close_recv_half(&mut self, id: StreamId) -> Result<(), ConnectionError> {
        let fully_closed = self.get_active_mut(id)
            .map(|s| s.close_recv_half())
            .unwrap_or_else(|| Err(ProtocolError.into()))?;

        if fully_closed {
            self.remove_active(id);
            self.reset.insert(id, NoError);
        }
        Ok(())
    }

    fn reset_stream(&mut self, id: StreamId, cause: Reason) {
        self.remove_active(id);
        self.reset.insert(id, cause);
    }

    fn get_reset(&self, id: StreamId) -> Option<Reason> {
        self.reset.get(&id).map(|r| *r)
    }

    fn is_local_active(&self, id: StreamId) -> bool {
        self.local_active.contains_key(&id)
    }

    fn is_remote_active(&self, id: StreamId) -> bool {
        self.remote_active.contains_key(&id)
    }

    fn is_send_open(&mut self, id: StreamId) -> bool {
        match self.get_active(id) {
            Some(s) => s.is_send_open(),
            None => false,
        }
    }

    fn is_recv_open(&mut self, id: StreamId) -> bool  {
        match self.get_active(id) {
            Some(s) => s.is_recv_open(),
            None => false,
        }
    }

    fn local_active_len(&self) -> usize {
        self.local_active.len()
    }

    fn remote_active_len(&self) -> usize {
        self.remote_active.len()
    }

    fn update_inital_recv_window_size(&mut self, old_sz: WindowSize, new_sz: WindowSize) {
        if new_sz < old_sz {
            let decr = old_sz - new_sz;

            for s in self.local_active.values_mut() {
                if let Some(fc) = s.recv_flow_controller() {
                    fc.shrink_window(decr);
                }
            }

            for s in self.remote_active.values_mut() {
                if let Some(fc) = s.recv_flow_controller() {
                    fc.shrink_window(decr);
                }
            }
        } else {
            let incr = new_sz - old_sz;

            for s in self.local_active.values_mut() {
                if let Some(fc) = s.recv_flow_controller() {
                    fc.expand_window(incr);
                }
            }

            for s in self.remote_active.values_mut() {
                if let Some(fc) = s.recv_flow_controller() {
                    fc.expand_window(incr);
                }
            }
        }
    }

    fn update_inital_send_window_size(&mut self, old_sz: WindowSize, new_sz: WindowSize) {
        if new_sz < old_sz {
            let decr = old_sz - new_sz;

            for s in self.local_active.values_mut() {
                if let Some(fc) = s.send_flow_controller() {
                    fc.shrink_window(decr);
                }
            }

            for s in self.remote_active.values_mut() {
                if let Some(fc) = s.send_flow_controller() {
                    fc.shrink_window(decr);
                }
            }
        } else {
            let incr = new_sz - old_sz;

            for s in self.local_active.values_mut() {
                if let Some(fc) = s.send_flow_controller() {
                    fc.expand_window(incr);
                }
            }

            for s in self.remote_active.values_mut() {
                if let Some(fc) = s.send_flow_controller() {
                    fc.expand_window(incr);
                }
            }
        }
    }

    fn recv_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState> {
        if id.is_zero() {
            None
        } else if P::is_valid_local_stream_id(id) {
            self.local_active.get_mut(&id).and_then(|s| s.recv_flow_controller())
        } else {
            self.remote_active.get_mut(&id).and_then(|s| s.recv_flow_controller())
        }
    }

    fn send_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState> {
        if id.is_zero() {
            None
        } else if P::is_valid_local_stream_id(id) {
            self.local_active.get_mut(&id).and_then(|s| s.send_flow_controller())
        } else {
            self.remote_active.get_mut(&id).and_then(|s| s.send_flow_controller())
        }
    }
}

/// Proxy.
impl<T, P> Stream for StreamStore<T, P>
    where T: Stream<Item = Frame, Error = ConnectionError>,
{
    type Item = Frame;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Option<Frame>, ConnectionError> {
        self.inner.poll()
    }
}

/// Proxy.
impl<T, P, U> Sink for StreamStore<T, P>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
{
    type SinkItem = Frame<U>;
    type SinkError = ConnectionError;

    fn start_send(&mut self, item: Self::SinkItem) -> StartSend<Frame<U>, ConnectionError> {
        self.inner.start_send(item)
    }

    fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
        self.inner.poll_complete()
    }
}

/// Proxy.
impl<T, P, U> ReadySink for StreamStore<T, P>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ReadySink,
{
    fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        self.inner.poll_ready()
    }
}

/// Proxy.
impl<T: ApplySettings, P> ApplySettings for StreamStore<T, P> {
    fn apply_local_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_local_settings(set)
    }

    fn apply_remote_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_remote_settings(set)
    }
}

/// Proxy.
impl<T: ControlPing, P> ControlPing for StreamStore<T, P> {
    fn start_ping(&mut self, body: PingPayload) -> StartSend<PingPayload, ConnectionError> {
        self.inner.start_ping(body)
    }

    fn take_pong(&mut self) -> Option<PingPayload> {
        self.inner.take_pong()
    }
}
