use {ConnectionError, Peer, StreamId};
use error::Reason::{NoError, ProtocolError};
use proto::*;
use proto::stream_state::StreamState;

use fnv::FnvHasher;
use ordermap::OrderMap;
use std::hash::BuildHasherDefault;

/// Holds the underlying stream state to be accessed by upper layers.
// TODO track reserved streams
// TODO constrain the size of `reset`
#[derive(Debug)]
pub struct StreamStates<T> {
    inner: T,
    streams: Streams,
}

#[derive(Debug)]
pub struct Streams {
    /// True when in the context of an H2 server.
    is_server: bool,

    /// Holds active streams initiated by the local endpoint.
    local_active: OrderMap<StreamId, StreamState, BuildHasherDefault<FnvHasher>>,

    /// Holds active streams initiated by the remote endpoint.
    remote_active: OrderMap<StreamId, StreamState, BuildHasherDefault<FnvHasher>>,

    /// Holds active streams initiated by the remote.
    reset: OrderMap<StreamId, Reason, BuildHasherDefault<FnvHasher>>,
}

impl<T, U> StreamStates<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
{
    pub fn new<P: Peer>(inner: T) -> StreamStates<T> {
        StreamStates {
            inner,
            streams: Streams {
                is_server: P::is_server(),
                local_active: OrderMap::default(),
                remote_active: OrderMap::default(),
                reset: OrderMap::default(),
            },
        }
    }
}

impl<T> ControlStreams for StreamStates<T> {
    fn streams(&self) -> &Streams {
        &self.streams
    }

    fn streams_mut(&mut self) -> &mut Streams {
        &mut self.streams
    }
}

impl Streams {
    pub fn is_valid_local_stream_id(&self, id: StreamId) -> bool {
        if self.is_server {
            id.is_server_initiated()
        } else {
            id.is_client_initiated()
        }
    }

    pub fn is_valid_remote_stream_id(&self, id: StreamId) -> bool {
        if self.is_server {
            id.is_client_initiated()
        } else {
            id.is_server_initiated()
        }
    }

    pub fn get_active(&self, id: StreamId) -> Option<&StreamState> {
        assert!(!id.is_zero());

        if self.is_valid_local_stream_id(id) {
            self.local_active.get(&id)
        } else {
            self.remote_active.get(&id)
        }
    }

    pub fn get_active_mut(&mut self, id: StreamId) -> Option<&mut StreamState> {
        assert!(!id.is_zero());

        if self.is_valid_local_stream_id(id) {
            self.local_active.get_mut(&id)
        } else {
            self.remote_active.get_mut(&id)
        }
    }

    pub fn remove_active(&mut self, id: StreamId) {
        assert!(!id.is_zero());

        if self.is_valid_local_stream_id(id) {
            self.local_active.remove(&id);
        } else {
            self.remote_active.remove(&id);
        }
    }

    pub fn can_local_open(&self) -> bool {
        !self.is_server
    }

    pub fn can_remote_open(&self) -> bool {
        !self.can_local_open()
    }

    pub fn local_open(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError> {
        if !self.is_valid_local_stream_id(id) || !self.can_local_open() {
            return Err(ProtocolError.into());
        }

        if self.local_active.contains_key(&id) {
            return Err(ProtocolError.into());
        }

        self.local_active.insert(id, StreamState::new_open_sending(sz));
        Ok(())
    }

    pub fn remote_open(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError> {
        if !self.is_valid_remote_stream_id(id) || !self.can_remote_open() {
            return Err(ProtocolError.into());
        }
        if self.remote_active.contains_key(&id) {
            return Err(ProtocolError.into());
        }

        self.remote_active.insert(id, StreamState::new_open_recving(sz));
        Ok(())
    }

    pub fn local_open_recv_half(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError> {
        if !self.is_valid_local_stream_id(id) {
            return Err(ProtocolError.into());
        }

        match self.local_active.get_mut(&id) {
            Some(s) => s.open_recv_half(sz).map(|_| {}),
            None => Err(ProtocolError.into()),
        }
    }

    pub fn remote_open_send_half(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError> {
        if !self.is_valid_remote_stream_id(id) {
            return Err(ProtocolError.into());
        }

        match self.remote_active.get_mut(&id) {
            Some(s) => s.open_send_half(sz).map(|_| {}),
            None => Err(ProtocolError.into()),
        }
    }

    pub fn close_send_half(&mut self, id: StreamId) -> Result<(), ConnectionError> {
        let fully_closed = self.get_active_mut(id)
            .map(|s| s.close_send_half())
            .unwrap_or_else(|| Err(ProtocolError.into()))?;

        if fully_closed {
            self.remove_active(id);
            self.reset.insert(id, NoError);
        }
        Ok(())
    }

    pub fn close_recv_half(&mut self, id: StreamId) -> Result<(), ConnectionError> {
        let fully_closed = self.get_active_mut(id)
            .map(|s| s.close_recv_half())
            .unwrap_or_else(|| Err(ProtocolError.into()))?;

        if fully_closed {
            self.remove_active(id);
            self.reset.insert(id, NoError);
        }
        Ok(())
    }

    pub fn reset_stream(&mut self, id: StreamId, cause: Reason) {
        self.remove_active(id);
        self.reset.insert(id, cause);
    }

    pub fn get_reset(&self, id: StreamId) -> Option<Reason> {
        self.reset.get(&id).map(|r| *r)
    }

    pub fn is_local_active(&self, id: StreamId) -> bool {
        self.local_active.contains_key(&id)
    }

    pub fn is_remote_active(&self, id: StreamId) -> bool {
        self.remote_active.contains_key(&id)
    }

    /// Returns true if the given stream was opened and is not yet closed.
    pub fn is_active(&self, id: StreamId) -> bool {
        if self.is_valid_local_stream_id(id) {
            self.is_local_active(id)
        } else {
            self.is_remote_active(id)
        }
    }

    pub fn is_send_open(&self, id: StreamId) -> bool {
        match self.get_active(id) {
            Some(s) => s.is_send_open(),
            None => false,
        }
    }

    pub fn is_recv_open(&self, id: StreamId) -> bool  {
        match self.get_active(id) {
            Some(s) => s.is_recv_open(),
            None => false,
        }
    }

    pub fn local_active_len(&self) -> usize {
        self.local_active.len()
    }

    pub fn remote_active_len(&self) -> usize {
        self.remote_active.len()
    }

    pub fn update_inital_recv_window_size(&mut self, old_sz: WindowSize, new_sz: WindowSize) {
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

    pub fn update_inital_send_window_size(&mut self, old_sz: WindowSize, new_sz: WindowSize) {
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

    pub fn recv_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState> {
        // TODO: Abstract getting the state for a stream ID
        if id.is_zero() {
            None
        } else if self.is_valid_local_stream_id(id) {
            self.local_active.get_mut(&id).and_then(|s| s.recv_flow_controller())
        } else {
            self.remote_active.get_mut(&id).and_then(|s| s.recv_flow_controller())
        }
    }

    pub fn send_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState> {
        if id.is_zero() {
            None
        } else if self.is_valid_local_stream_id(id) {
            self.local_active.get_mut(&id).and_then(|s| s.send_flow_controller())
        } else {
            self.remote_active.get_mut(&id).and_then(|s| s.send_flow_controller())
        }
    }
}

proxy_apply_settings!(StreamStates);
proxy_control_ping!(StreamStates);
proxy_stream!(StreamStates);
proxy_sink!(StreamStates);
proxy_ready_sink!(StreamStates);
