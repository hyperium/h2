use {ConnectionError, Peer, StreamId};
use error::Reason::{NoError, ProtocolError};
use proto::*;
use proto::state::{StreamState, PeerState};

use fnv::FnvHasher;
use ordermap::OrderMap;
use std::hash::BuildHasherDefault;
use std::marker::PhantomData;

/// Exposes stream states to "upper" layers of the transport (i.e. from StreamTracker up
/// to Connection).
pub trait ControlStreams {
    fn local_valid_id(id: StreamId) -> bool;
    fn remote_valid_id(id: StreamId) -> bool;

    fn local_can_open() -> bool;
    fn remote_can_open() -> bool {
        !Self::local_can_open()
    }

    fn local_open(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError>;
    fn remote_open(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError>;

    // fn local_reserve(&mut self, id: StreamId) -> Result<(), ConnectionError>;
    // fn remote_reserve(&mut self, id: StreamId) -> Result<(), ConnectionError>;

    fn close_local_half(&mut self, id: StreamId) -> Result<(), ConnectionError>;
    fn close_remote_half(&mut self, id: StreamId) -> Result<(), ConnectionError>;

    fn reset_stream(&mut self, id: StreamId, cause: Reason);
    fn get_reset(&self, id: StreamId) -> Option<Reason>;

    fn is_local_active(&self, id: StreamId) -> bool;
    fn is_remote_active(&self, id: StreamId) -> bool;

    fn local_active_len(&self) -> usize;
    fn remote_active_len(&self) -> usize;

    fn local_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState>;
    fn remote_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState>;

    fn local_update_inital_window_size(&mut self, old_sz: u32, new_sz: u32);
    fn remote_update_inital_window_size(&mut self, old_sz: u32, new_sz: u32);

    fn check_can_send_data(&mut self, id: StreamId) -> Result<(), ConnectionError>;
    fn check_can_recv_data(&mut self, id: StreamId) -> Result<(), ConnectionError>;
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
    //
    fn local_open(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError> {
        debug_assert!(Self::local_can_open());
        assert!(Self::local_valid_id(id));
        debug_assert!(!self.local_active.contains_key(&id));

        self.local_active.insert(id, StreamState::Open {
            remote: PeerState::Data(FlowControlState::with_initial_size(sz)),
            local: PeerState::Headers,
        });
        Ok(())
    }

    /// Open a new stream from the remote side (i.e. as a Server).
    fn remote_open(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError> {
        debug_assert!(Self::remote_can_open());
        assert!(Self::remote_valid_id(id));
        debug_assert!(!self.remote_active.contains_key(&id));

        self.remote_active.insert(id, StreamState::Open {
            local: PeerState::Data(FlowControlState::with_initial_size(sz)),
            remote: PeerState::Headers,
        });
        Ok(())
    }

    fn close_local_half(&mut self, id: StreamId) -> Result<(), ConnectionError> {
        let fully_closed = self.get_active_mut(id)
            .map(|s| s.close_local())
            .unwrap_or_else(|| Err(ProtocolError.into()))?;

        if fully_closed {
            self.remove_active(id);
            self.reset.insert(id, NoError);
        }
        Ok(())
    }

    fn close_remote_half(&mut self, id: StreamId) -> Result<(), ConnectionError> {
        let fully_closed = self.get_active_mut(id)
            .map(|s| s.close_remote())
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

    fn local_update_inital_window_size(&mut self, old_sz: u32, new_sz: u32) {
        if new_sz < old_sz {
            let decr = old_sz - new_sz;

            for s in self.local_active.values_mut() {
                if let Some(fc) = s.local_flow_controller() {
                    fc.shrink_window(decr);
                }
            }

            for s in self.remote_active.values_mut() {
                if let Some(fc) = s.local_flow_controller() {
                    fc.shrink_window(decr);
                }
            }
        } else {
            let incr = new_sz - old_sz;

            for s in self.local_active.values_mut() {
                if let Some(fc) = s.local_flow_controller() {
                    fc.expand_window(incr);
                }
            }

            for s in self.remote_active.values_mut() {
                if let Some(fc) = s.local_flow_controller() {
                    fc.expand_window(incr);
                }
            }
        }
    }

    fn remote_update_inital_window_size(&mut self, old_sz: u32, new_sz: u32) {
        if new_sz < old_sz {
            let decr = old_sz - new_sz;

            for s in self.local_active.values_mut() {
                if let Some(fc) = s.remote_flow_controller() {
                    fc.shrink_window(decr);
                }
            }

            for s in self.remote_active.values_mut() {
                if let Some(fc) = s.remote_flow_controller() {
                    fc.shrink_window(decr);
                }
            }
        } else {
            let incr = new_sz - old_sz;

            for s in self.local_active.values_mut() {
                if let Some(fc) = s.remote_flow_controller() {
                    fc.expand_window(incr);
                }
            }

            for s in self.remote_active.values_mut() {
                if let Some(fc) = s.remote_flow_controller() {
                    fc.expand_window(incr);
                }
            }
        }
    }

    fn local_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState> {
        if id.is_zero() {
            None
        } else if P::is_valid_local_stream_id(id) {
            self.local_active.get_mut(&id).and_then(|s| s.local_flow_controller())
        } else {
            self.remote_active.get_mut(&id).and_then(|s| s.local_flow_controller())
        }
    }

    fn remote_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState> {
        if id.is_zero() {
            None
        } else if P::is_valid_local_stream_id(id) {
            self.local_active.get_mut(&id).and_then(|s| s.remote_flow_controller())
        } else {
            self.remote_active.get_mut(&id).and_then(|s| s.remote_flow_controller())
        }
    }

    fn check_can_send_data(&mut self, id: StreamId) -> Result<(), ConnectionError> {
        if let Some(s) = self.get_active(id) {
            return s.check_can_send_data();
        }
        Err(ProtocolError.into())
    }

    fn check_can_recv_data(&mut self, id: StreamId) -> Result<(), ConnectionError>  {
        if let Some(s) = self.get_active(id) {
            return s.check_can_recv_data();
        }
        Err(ProtocolError.into())
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
