use Peer;
use error::ConnectionError;
use error::Reason::*;
use error::User::*;
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
    /// Transition the state to represent headers being received.
    ///
    /// Returns true if this state transition results in iniitializing the
    /// stream id. `Err` is returned if this is an invalid state transition.
    pub fn recv_headers<P: Peer>(&mut self,
                                 eos: bool,
                                 initial_recv_window_size: WindowSize)
        -> Result<bool, ConnectionError>
    {
        use self::StreamState::*;
        use self::PeerState::*;

        match *self {
            Idle => {
                let local = Headers;
                if eos {
                    *self = HalfClosedRemote(local);
                } else {
                    *self = Open { local, remote: Data(FlowControlState::with_initial_size(initial_recv_window_size)) };
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
                    *self = HalfClosedLocal(Data(FlowControlState::with_initial_size(initial_recv_window_size)));
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
