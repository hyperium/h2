use {ConnectionError, Peer, StreamId};
use error::Reason;
use proto::*;

use fnv::FnvHasher;
use ordermap::OrderMap;
use std::hash::BuildHasherDefault;
use std::marker::PhantomData;

/// Exposes stream states to "upper" layers of the transport (i.e. from StreamTracker up
/// to Connection).
pub trait ControlStreams {
    fn is_valid_local_id(id: StreamId) -> bool;
    fn is_valid_remote_id(id: StreamId) -> bool;

    fn can_create_local_stream() -> bool;
    fn can_create_remote_stream() -> bool {
        !Self::can_create_local_stream()
    }

    fn get_reset(&self, id: StreamId) -> Option<Reason>;
    fn reset_stream(&mut self, id: StreamId, cause: Reason);

    fn is_local_active(&self, id: StreamId) -> bool;
    fn is_remote_active(&self, id: StreamId) -> bool;

    fn local_active_len(&self) -> usize;
    fn remote_active_len(&self) -> usize;

    fn local_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState>;
    fn remote_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState>;

    fn local_update_inital_window_size(&mut self, old_sz: u32, new_sz: u32);
    fn remote_update_inital_window_size(&mut self, old_sz: u32, new_sz: u32);

    // fn get_active(&self, id: StreamId) -> Option<&StreamState> {
    //     self.streams(id).get_active(id)
    // }

    // fn get_active_mut(&mut self, id: StreamId) -> Option<&mut StreamState>  {
    //     self.streams_mut(id).get_active_mut(id)
    // }
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

impl<T, P: Peer> ControlStreams for StreamStore<T, P> {
    fn is_valid_local_id(id: StreamId) -> bool {
        P::is_valid_local_stream_id(id)
    }

    fn is_valid_remote_id(id: StreamId) -> bool {
        P::is_valid_remote_stream_id(id)
    }

    fn can_create_local_stream() -> bool {
        P::can_create_local_stream()
    }

    fn get_reset(&self, id: StreamId) -> Option<Reason> {
        self.reset.get(&id).map(|r| *r)
    }

    fn reset_stream(&mut self, id: StreamId, cause: Reason) {
        if P::is_valid_local_stream_id(id) {
            self.local_active.remove(&id);
        } else {
            self.remote_active.remove(&id);
        }
        self.reset.insert(id, cause);
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
}

impl<T: ApplySettings, P> ApplySettings for StreamStore<T, P> {
    fn apply_local_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_local_settings(set)
    }

    fn apply_remote_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_remote_settings(set)
    }
}
