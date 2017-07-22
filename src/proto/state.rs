use ConnectionError;
use error::Reason::*;
use proto::{FlowControlState, WindowSize};

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
    pub fn new_open_sending(sz: WindowSize) -> StreamState {
        StreamState::Open {
            local: PeerState::AwaitingHeaders,
            remote: PeerState::streaming(sz),
        }
    }

    pub fn new_open_recving(sz: WindowSize) -> StreamState {
        StreamState::Open {
            local: PeerState::streaming(sz),
            remote: PeerState::AwaitingHeaders,
        }
    }

    /// Opens the send-half of a stream if it is not already open.
    ///
    /// Returns true iff the send half was not previously open.
    pub fn open_send_half(&mut self, sz: WindowSize) -> Result<bool, ConnectionError> {
        use self::StreamState::*;
        use self::PeerState::*;

        // Try to avoid copying `self` by first checking to see whether the stream needs
        // to be updated.
        match self {
            &mut Idle |
            &mut Closed |
            &mut HalfClosedRemote(..) => {
                return Err(ProtocolError.into());
            }

            &mut Open { remote: Streaming(..), .. } |
            &mut HalfClosedLocal(Streaming(..)) => {
                return Ok(false);
            }

            &mut Open { remote: AwaitingHeaders, .. } |
            &mut HalfClosedLocal(AwaitingHeaders) => {}
        }

        match *self {
            Open { local, remote: AwaitingHeaders } => {
                *self = Open {
                    local,
                    remote: PeerState::streaming(sz),
                };
            }

            HalfClosedLocal(AwaitingHeaders) => {
                *self = HalfClosedLocal(PeerState::streaming(sz));
            }

            _ => unreachable!()
        }

        Ok(true)
    }

    pub fn open_recv_half(&mut self, sz: WindowSize) -> Result<bool, ConnectionError> {
        use self::StreamState::*;
        use self::PeerState::*;

        // Try to avoid copying `self` by first checking to see whether the stream needs
        // to be updated.
        match self {
            &mut Idle |
            &mut Closed |
            &mut HalfClosedLocal(..) => {
                return Err(ProtocolError.into());
            }

            &mut Open { local: Streaming(..), .. } |
            &mut HalfClosedRemote(Streaming(..)) => {
                return Ok(false);
            }

            &mut Open { local: AwaitingHeaders, .. } |
            &mut HalfClosedRemote(AwaitingHeaders) => {}
        }

        match *self {
            Open { remote, local: AwaitingHeaders } => {
                *self = Open {
                    local: PeerState::streaming(sz),
                    remote,
                };
            }

            HalfClosedRemote(AwaitingHeaders) => {
                *self = HalfClosedRemote(PeerState::streaming(sz));
            }

            _ => unreachable!()
        }

        Ok(true)
    }

    pub fn can_send_data(&self) -> bool {
        use self::StreamState::*;
        match self {
            &Idle | &Closed | &HalfClosedRemote(..) => false,

            &Open { ref remote, .. } |
            &HalfClosedLocal(ref remote) => remote.is_streaming(),
        }
    }

    pub fn can_recv_data(&self) -> bool {
        use self::StreamState::*;
        match self {
            &Idle | &Closed | &HalfClosedLocal(..) => false,

            &Open { ref local, .. } |
            &HalfClosedRemote(ref local) => {
                local.is_streaming()
            }
        }
    }

    /// Indicates that the local side will not send more data to the remote.
    ///
    /// Returns true iff the stream is fully closed.
    pub fn close_send_half(&mut self) -> Result<bool, ConnectionError> {
        use self::StreamState::*;
        match *self {
            Open { local, .. } => {
                // The local side will continue to receive data.
                trace!("close_send_half: Open => HalfClosedRemote({:?})", local);
                *self = HalfClosedRemote(local);
                Ok(false)
            }

            HalfClosedLocal(..) => {
                trace!("close_send_half: HalfClosedLocal => Closed");
                *self = Closed;
                Ok(true)
            }

            Idle | Closed | HalfClosedRemote(..) => {
                Err(ProtocolError.into())
            }
        }
    }

    /// Indicates that the remote side will not send more data to the local.
    ///
    /// Returns true iff the stream is fully closed.
    pub fn close_recv_half(&mut self) -> Result<bool, ConnectionError> {
        use self::StreamState::*;
        match *self {
            Open { remote, .. } => {
                // The remote side will continue to receive data.
                trace!("close_recv_half: Open => HalfClosedLocal({:?})", remote);
                *self = HalfClosedLocal(remote);
                Ok(false)
            }

            HalfClosedRemote(..) => {
                trace!("close_recv_half: HalfClosedRemoteOpen => Closed");
                *self = Closed;
                Ok(true)
            }

            Idle | Closed | HalfClosedLocal(..) => {
                Err(ProtocolError.into())
            }
        }
    }

    pub fn recv_flow_controller(&mut self) -> Option<&mut FlowControlState> {
        use self::StreamState::*;
        match self {
            &mut Open { ref mut local, .. } |
            &mut HalfClosedRemote(ref mut local) => local.flow_controller(),
            _ => None,
        }
    }

    pub fn send_flow_controller(&mut self) -> Option<&mut FlowControlState> {
        use self::StreamState::*;
        match self {
            &mut Open { ref mut remote, .. } |
            &mut HalfClosedLocal(ref mut remote) => remote.flow_controller(),
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
    AwaitingHeaders,
    /// Contains a FlowControlState representing the _receiver_ of this this data stream.
    Streaming(FlowControlState),
}

impl Default for PeerState {
    fn default() -> Self {
        PeerState::AwaitingHeaders
    }
}

impl PeerState {
    fn streaming(sz: WindowSize) -> PeerState {
        PeerState::Streaming(FlowControlState::with_initial_size(sz))
    }

    #[inline]
    fn is_streaming(&self) -> bool {
        use self::PeerState::*;
        match self {
            &Streaming(..) => true,
            _ => false,
        }
    }

    fn flow_controller(&mut self) -> Option<&mut FlowControlState> {
        use self::PeerState::*;
        match self {
            &mut Streaming(ref mut fc) => Some(fc),
            _ => None,
        }
    }
}
