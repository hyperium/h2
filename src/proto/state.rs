use Peer;
use error::ConnectionError;
use error::Reason::*;
use error::User::*;
use proto::FlowController;

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
pub enum State {
    Idle,
    ReservedLocal,
    ReservedRemote,
    Open {
        local: PeerState,
        remote: PeerState,
    },
    HalfClosedLocal(PeerState),
    HalfClosedRemote(PeerState),
    Closed,
}

#[derive(Debug, Copy, Clone)]
pub enum PeerState {
    Headers,
    Data(FlowController),
}

impl State {
    pub fn increment_local_window_size(&mut self, incr: u32) {
        use self::State::*;
        use self::PeerState::*;

        *self = match *self {
            Open { local: Data(mut local), remote } => {
                local.increment(incr);
                Open { local: Data(local), remote }
            }
            HalfClosedRemote(Data(mut local)) => {
                local.increment(incr);
                HalfClosedRemote(Data(local))
            }
            s => s,
        }
    }

    /// Transition the state to represent headers being received.
    ///
    /// Returns true if this state transition results in iniitializing the
    /// stream id. `Err` is returned if this is an invalid state transition.
    pub fn recv_headers<P: Peer>(&mut self, eos: bool, remote_window_size: u32) -> Result<bool, ConnectionError> {
        use self::State::*;
        use self::PeerState::*;

        match *self {
            Idle => {
                *self = if eos {
                    HalfClosedRemote(Headers)
                } else {
                    Open {
                        local: Headers,
                        remote: Data(FlowController::new(remote_window_size)),
                    }
                };

                Ok(true)
            }
            Open { local, remote } => {
                try!(remote.check_is_headers(ProtocolError.into()));

                *self = if eos {
                    HalfClosedRemote(local)
                } else {
                    let remote = Data(FlowController::new(remote_window_size));
                    Open { local, remote }
                };

                Ok(false)
            }
            HalfClosedLocal(remote) => {
                try!(remote.check_is_headers(ProtocolError.into()));

                *self = if eos {
                    Closed
                } else {
                    HalfClosedLocal(Data(FlowController::new(remote_window_size)))
                };

                Ok(false)
            }
            Closed | HalfClosedRemote(..) => {
                Err(ProtocolError.into())
            }
            _ => unimplemented!(),
        }
    }

    pub fn recv_data(&mut self, eos: bool) -> Result<(), ConnectionError> {
        use self::State::*;

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
    pub fn send_headers<P: Peer>(&mut self, eos: bool, local_window_size: u32) -> Result<bool, ConnectionError> {
        use self::State::*;
        use self::PeerState::*;

        match *self {
            Idle => {
                *self = if eos {
                    HalfClosedLocal(Headers)
                } else {
                    Open {
                        local: Data(FlowController::new(local_window_size)),
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
                    let local = Data(FlowController::new(local_window_size));
                    Open { local, remote }
                };

                Ok(false)
            }
            HalfClosedRemote(local) => {
                try!(local.check_is_headers(UnexpectedFrameType.into()));

                *self = if eos {
                    Closed
                } else {
                    HalfClosedRemote(Data(FlowController::new(local_window_size)))
                };

                Ok(false)
            }
            Closed | HalfClosedLocal(..) => {
                Err(UnexpectedFrameType.into())
            }
            _ => unimplemented!(),
        }
    }

    pub fn send_data(&mut self, eos: bool) -> Result<(), ConnectionError> {
        use self::State::*;

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
            Closed | HalfClosedLocal(..) => {
                Err(UnexpectedFrameType.into())
            }
            _ => unimplemented!(),
        }
    }
}

impl PeerState {
    #[inline]
    fn check_is_headers(&self, err: ConnectionError) -> Result<(), ConnectionError> {
        use self::PeerState::*;

        match *self {
            Headers => Ok(()),
            _ => Err(err),
        }
    }

    #[inline]
    fn check_is_data(&self, err: ConnectionError) -> Result<(), ConnectionError> {
        use self::PeerState::*;

        match *self {
            Data { .. } => Ok(()),
            _ => Err(err),
        }
    }
}

impl Default for State {
    fn default() -> State {
        State::Idle
    }
}
