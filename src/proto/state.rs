use {Peer, StreamId};
use error::{ConnectionError, Reason};
use error::Reason::*;
use error::User::*;
use proto::{FlowControlState, WindowSize};

use fnv::FnvHasher;
use ordermap::{Entry, OrderMap};
use std::hash::BuildHasherDefault;
use std::marker::PhantomData;

/// Represents the state of an H2 stream
///
/// ```not_rust
///                              +--------+
///                      send PP |        | recv PP
///                     ,--------|  idle  |--------.
///                    /         |        |         \
///                   v          +--------+          v
///            +----------+          |           +----------+
///            |          |          | send H /  |          |
///     ,------| reserved |          | recv H    | reserved |------.
///     |      | (local)  |          |           | (remote) |      |
///     |      +----------+          v           +----------+      |
///     |          |             +--------+             |          |
///     |          |     recv ES |        | send ES     |          |
///     |   send H |     ,-------|  open  |-------.     | recv H   |
///     |          |    /        |        |        \    |          |
///     |          v   v         +--------+         v   v          |
///     |      +----------+          |           +----------+      |
///     |      |   half   |          |           |   half   |      |
///     |      |  closed  |          | send R /  |  closed  |      |
///     |      | (remote) |          | recv R    | (local)  |      |
///     |      +----------+          |           +----------+      |
///     |           |                |                 |           |
///     |           | send ES /      |       recv ES / |           |
///     |           | send R /       v        send R / |           |
///     |           | recv R     +--------+   recv R   |           |
///     | send R /  `----------->|        |<-----------'  send R / |
///     | recv R                 | closed |               recv R   |
///     `----------------------->|        |<----------------------'
///                              +--------+
///
///        send:   endpoint sends this frame
///        recv:   endpoint receives this frame
///
///        H:  HEADERS frame (with implied CONTINUATIONs)
///        PP: PUSH_PROMISE frame (with implied CONTINUATIONs)
///        ES: END_STREAM flag
///        R:  RST_STREAM frame
/// ```
#[derive(Debug, Copy, Clone)]
pub enum StreamState {
    Idle,
    // TODO: these states shouldn't count against concurrency limits:
    //ReservedLocal,
    //ReservedRemote,
    Open {
        local: PeerState,
        remote: PeerState,
    },
    HalfClosedLocal(PeerState),
    HalfClosedRemote(PeerState),
    Closed,
}

impl StreamState {
    pub fn is_closed(&self) ->  bool {
        use self::StreamState::*;

        match self {
            &Closed => true,
            _ => false,
        }
    }
    /// Transition the state to represent headers being received.
    ///
    /// Returns true if this state transition results in iniitializing the
    /// stream id. `Err` is returned if this is an invalid state transition.
    pub fn recv_headers<P>(&mut self, eos: bool, initial_window_size: WindowSize)
        -> Result<bool, ConnectionError>
        where P: Peer
    {
        use self::StreamState::*;
        use self::PeerState::*;

        match *self {
            Idle => {
                let local = Headers;
                if eos {
                    *self = HalfClosedRemote(local);
                } else {
                    let remote = Data(FlowControlState::with_initial_size(initial_window_size));
                    *self = Open { local, remote };
                }
                Ok(true)
            }

            Open { local, remote } => {
                try!(remote.check_is_headers(ProtocolError.into()));
                if !eos {
                    // Received non-trailers HEADERS on open remote.
                    return Err(ProtocolError.into());
                }
                *self = HalfClosedRemote(local);
                Ok(false)
            }

            HalfClosedLocal(headers) => {
                try!(headers.check_is_headers(ProtocolError.into()));
                if eos {
                    *self = Closed;
                } else {
                    let remote = FlowControlState::with_initial_size(initial_window_size);
                    *self = HalfClosedLocal(Data(remote));
                };
                Ok(false)
            }

            Closed | HalfClosedRemote(..) => {
                Err(ProtocolError.into())
            }
        }
    }

    pub fn recv_data(&mut self, eos: bool) -> Result<(), ConnectionError> {
        use self::StreamState::*;

        match *self {
            Open { local, remote } => {
                try!(remote.check_is_data(ProtocolError.into()));
                if eos {
                    *self = HalfClosedRemote(local);
                }
                Ok(())
            }

            HalfClosedLocal(remote) => {
                try!(remote.check_is_data(ProtocolError.into()));
                if eos {
                    *self = Closed;
                }
                Ok(())
            }

            Closed | HalfClosedRemote(..) => {
                Err(ProtocolError.into())
            }

            _ => unimplemented!(),
        }
    }

    /// Transition the state to represent headers being sent.
    ///
    /// Returns true if this state transition results in initializing the stream
    /// id. `Err` is returned if this is an invalid state transition.
    pub fn send_headers<P: Peer>(&mut self, 
                                 eos: bool,
                                 initial_window_size: WindowSize)
        -> Result<bool, ConnectionError>
    {
        use self::StreamState::*;
        use self::PeerState::*;

        match *self {
            Idle => {
                *self = if eos {
                    HalfClosedLocal(Headers)
                } else {
                    Open {
                        local: Data(FlowControlState::with_initial_size(initial_window_size)),
                        remote: Headers,
                    }
                };

                Ok(true)
            }

            Open { local, remote } => {
                try!(local.check_is_headers(UnexpectedFrameType.into()));

                *self = if eos {
                    HalfClosedLocal(remote)
                } else {
                    let fc = FlowControlState::with_initial_size(initial_window_size);
                    let local = Data(fc);
                    Open { local, remote }
                };

                Ok(false)
            }

            HalfClosedRemote(local) => {
                try!(local.check_is_headers(UnexpectedFrameType.into()));

                *self = if eos {
                    Closed
                } else {
                    let fc = FlowControlState::with_initial_size(initial_window_size);
                    HalfClosedRemote(Data(fc))
                };

                Ok(false)
            }

            Closed | HalfClosedLocal(..) => {
                Err(UnexpectedFrameType.into())
            }
        }
    }

    pub fn send_data(&mut self, eos: bool) -> Result<(), ConnectionError> {
        use self::StreamState::*;

        match *self {
            Open { local, remote } => {
                try!(local.check_is_data(UnexpectedFrameType.into()));
                if eos {
                    *self = HalfClosedLocal(remote);
                }
                Ok(())
            }

            HalfClosedRemote(local) => {
                try!(local.check_is_data(UnexpectedFrameType.into()));
                if eos {
                    *self = Closed;
                }
                Ok(())
            }

            Idle | Closed | HalfClosedLocal(..) => {
                Err(UnexpectedFrameType.into())
            }
        }
    }
 
    pub fn local_flow_controller(&mut self) -> Option<&mut FlowControlState> {
        use self::StreamState::*;
        use self::PeerState::*;

        match self {
            &mut Open { local: Data(ref mut fc), .. } |
            &mut HalfClosedRemote(Data(ref mut fc)) => Some(fc),
            _ => None,
        }
    }

    pub fn remote_flow_controller(&mut self) -> Option<&mut FlowControlState> {
        use self::StreamState::*;
        use self::PeerState::*;

        match self {
            &mut Open { remote: Data(ref mut fc), .. } |
            &mut HalfClosedLocal(Data(ref mut fc)) => Some(fc),
            _ => None,
        }
    }
}

impl Default for StreamState {
    fn default() -> StreamState {
        StreamState::Idle
    }
}

#[derive(Debug, Copy, Clone)]
pub enum PeerState {
    Headers,
    /// Contains a FlowControlState representing the _receiver_ of this this data stream.
    Data(FlowControlState),
}

impl PeerState {
    #[inline]
    fn check_is_headers(&self, err: ConnectionError) -> Result<(), ConnectionError> {
        use self::PeerState::*;
        match self {
            &Headers => Ok(()),
            _ => Err(err),
        }
    }

    #[inline]
    fn check_is_data(&self, err: ConnectionError) -> Result<(), ConnectionError> {
        use self::PeerState::*;
        match self {
            &Data(_) => Ok(()),
            _ => Err(err),
        }
    }
}

// TODO track reserved streams
// TODO constrain the size of `reset`
#[derive(Debug, Default)]
pub struct StreamMap<P> {
    /// Holds active streams initiated by the local endpoint.
    local_active: OrderMap<StreamId, StreamState, BuildHasherDefault<FnvHasher>>,

    /// Holds active streams initiated by the remote endpoint.
    remote_active: OrderMap<StreamId, StreamState, BuildHasherDefault<FnvHasher>>,

    /// Holds active streams initiated by the remote.
    reset: OrderMap<StreamId, Reason, BuildHasherDefault<FnvHasher>>,

    _phantom: PhantomData<P>,
}

impl<P: Peer> StreamMap<P> {
    pub fn active(&mut self, id: StreamId) -> Option<&StreamState> {
        assert!(!id.is_zero());
        if P::is_valid_local_stream_id(id) {
            self.local_active.get(id)
        } else {
            self.remote_active.get(id)
        }
    }

    pub fn active_mut(&mut self, id: StreamId) -> Option<&mut StreamState> {
        assert!(!id.is_zero());
        if P::is_valid_local_stream_id(id) {
            self.local_active.get_mut(id)
        } else {
            self.remote_active.get_mut(id)
        }
    }

    pub fn local_active(&self, id: StreamId) -> Option<&StreamState> {
        self.local_active.get(&id)
    }

    pub fn local_active_mut(&mut self, id: StreamId) -> Option<&mut StreamState> {
        self.local_active.get_mut(&id)
    }

    pub fn local_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState> {
        self.get_active_mut(id).and_then(|s| s.local_flow_controller())
    }

    pub fn remote_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState> {
        self.get_active_mut(id).and_then(|s| s.remote_flow_controller())
    }

    pub fn localis_active(&mut self, id: StreamId) -> bool {
        self.active.contains_key(&id)
    }

    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    pub fn reset(&mut self, id: StreamId, cause: Reason) {
        self.reset.insert(id, cause);
        self.active.remove(&id);
    }

    pub fn get_reset(&mut self, id: StreamId) -> Option<Reason> {
        self.reset.get(&id).map(|r| *r)
    }

    pub fn shrink_all_local_windows(&mut self, decr: u32) {
        for (_, mut s) in &mut self.active {
            if let Some(fc) = s.local_flow_controller() {
                fc.shrink_window(decr);
            }
        }
    }

    pub fn expand_all_local_windows(&mut self, incr: u32) {
        for (_, mut s) in &mut self.active {
            if let Some(fc) = s.local_flow_controller() {
                fc.expand_window(incr);
            }
        }
    }

    pub fn shrink_all_remote_windows(&mut self, decr: u32) {
        for (_, mut s) in &mut self.active {
            if let Some(fc) = s.remote_flow_controller() {
                fc.shrink_window(decr);
            }
        }
    }

    pub fn expand_all_remote_windows(&mut self, incr: u32) {
        for (_, mut s) in &mut self.active {
            if let Some(fc) = s.remote_flow_controller() {
                fc.expand_window(incr);
            }
        }
    }
}
