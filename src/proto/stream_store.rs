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
    fn local_valid_id(id: StreamId) -> bool;
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

    /// Create a new stream in the OPEN state from the local side (i.e. as a Client).
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

    // fn local_reserve(&mut self, id: StreamId) -> Result<(), ConnectionError>;
    // fn remote_reserve(&mut self, id: StreamId) -> Result<(), ConnectionError>;

    /// Close the local half of a stream so that the local side may not RECEIVE
    fn close_send_half(&mut self, id: StreamId) -> Result<(), ConnectionError>;
    fn close_recv_half(&mut self, id: StreamId) -> Result<(), ConnectionError>;

    fn reset_stream(&mut self, id: StreamId, cause: Reason);
    fn get_reset(&self, id: StreamId) -> Option<Reason>;

    fn is_local_active(&self, id: StreamId) -> bool;
    fn is_remote_active(&self, id: StreamId) -> bool;

    fn is_active(&self, id: StreamId) -> bool {
        if Self::local_valid_id(id) {
            self.is_local_active(id)
        } else {
            self.is_remote_active(id)
        }
    }

    fn local_active_len(&self) -> usize;
    fn remote_active_len(&self) -> usize;

    fn recv_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState>;
    fn send_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState>;

    fn update_inital_recv_window_size(&mut self, old_sz: u32, new_sz: u32);
    fn update_inital_send_window_size(&mut self, old_sz: u32, new_sz: u32);

    fn can_send_data(&mut self, id: StreamId) -> bool;
    fn can_recv_data(&mut self, id: StreamId) -> bool;
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

impl<T, P> Stream for StreamStore<T, P>
    where T: Stream<Item = Frame, Error = ConnectionError>,
{
    type Item = Frame;
    type Error = ConnectionError;

    fn poll(&mut self) -> Poll<Option<Frame>, ConnectionError> {
        self.inner.poll()
    }
}

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

impl<T, P, U> ReadySink for StreamStore<T, P>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ReadySink,
{
    fn poll_ready(&mut self) -> Poll<(), ConnectionError> {
        self.inner.poll_ready()
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

    /// Open a new stream from the local side (i.e. as a Client).
    fn local_open(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError> {
        assert!(Self::local_valid_id(id));
        debug_assert!(Self::local_can_open());
        debug_assert!(!self.local_active.contains_key(&id));

        self.local_active.insert(id, StreamState::new_open_sending(sz));
        Ok(())
    }

    /// Open a new stream from the remote side (i.e. as a Server).
    fn remote_open(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError> {
        assert!(Self::remote_valid_id(id));
        debug_assert!(Self::remote_can_open());
        debug_assert!(!self.remote_active.contains_key(&id));

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

    fn local_active_len(&self) -> usize {
        self.local_active.len()
    }

    fn remote_active_len(&self) -> usize {
        self.remote_active.len()
    }

    fn update_inital_recv_window_size(&mut self, old_sz: u32, new_sz: u32) {
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

    fn update_inital_send_window_size(&mut self, old_sz: u32, new_sz: u32) {
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

    fn can_send_data(&mut self, id: StreamId) -> bool {
        match self.get_active(id) {
            Some(s) => s.can_send_data(),
            None => false,
        }
    }

    fn can_recv_data(&mut self, id: StreamId) -> bool  {
        match self.get_active(id) {
            Some(s) => s.can_recv_data(),
            None => false,
        }
    }
}

impl<T: ApplySettings, P> ApplySettings for StreamStore<T, P> {
    fn apply_local_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_local_settings(set)
    }

    fn apply_remote_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_remote_settings(set)
    }
}

impl<T: ControlPing, P> ControlPing for StreamStore<T, P> {
    fn start_ping(&mut self, body: PingPayload) -> StartSend<PingPayload, ConnectionError> {
        self.inner.start_ping(body)
    }

    fn take_pong(&mut self) -> Option<PingPayload> {
        self.inner.take_pong()
    }
}
