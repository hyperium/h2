use {ConnectionError, Peer, StreamId};
use error::Reason::{NoError, ProtocolError};
use proto::*;
use proto::stream_state::StreamState;

use fnv::FnvHasher;
use ordermap::OrderMap;
use std::hash::BuildHasherDefault;
use std::marker::PhantomData;

/// Holds the underlying stream state to be accessed by upper layers.
// TODO track reserved streams
// TODO constrain the size of `reset`
#[derive(Debug, Default)]
pub struct StreamStates<T, P> {
    inner: T,

    /// Holds active streams initiated by the local endpoint.
    local_active: OrderMap<StreamId, StreamState, BuildHasherDefault<FnvHasher>>,

    /// Holds active streams initiated by the remote endpoint.
    remote_active: OrderMap<StreamId, StreamState, BuildHasherDefault<FnvHasher>>,

    /// Holds active streams initiated by the remote.
    reset: OrderMap<StreamId, Reason, BuildHasherDefault<FnvHasher>>,

    _phantom: PhantomData<P>,
}

impl<T, P, U> StreamStates<T, P>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          P: Peer,
{
    pub fn new(inner: T) -> StreamStates<T, P> {
        StreamStates {
            inner,
            local_active: OrderMap::default(),
            remote_active: OrderMap::default(),
            reset: OrderMap::default(),
            _phantom: PhantomData,
        }
    }
}

impl<T, P: Peer> StreamStates<T, P> {
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

impl<T, P: Peer> ControlStreams for StreamStates<T, P> {
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

proxy_apply_settings!(StreamStates, P);
proxy_control_ping!(StreamStates, P);
proxy_stream!(StreamStates, P);
proxy_sink!(StreamStates, P);
proxy_ready_sink!(StreamStates, P);
