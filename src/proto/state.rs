use {ConnectionError, Reason, Peer};

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
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
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

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum PeerState {
    Headers,
    Data,
}

impl State {
    /// Transition the state to represent headers being received.
    ///
    /// Returns true if this state transition results in iniitializing the
    /// stream id. `Err` is returned if this is an invalid state transition.
    pub fn recv_headers<P: Peer>(&mut self, eos: bool) -> Result<bool, ConnectionError> {
        use self::State::*;
        use self::PeerState::*;

        match *self {
            Idle => {
                *self = if eos {
                    HalfClosedRemote(Headers)
                } else {
                    Open {
                        local: Headers,
                        remote: Data,
                    }
                };

                Ok(true)
            }
            Open { local, remote } => {
                try!(remote.check_is_headers(Reason::ProtocolError));

                *self = if eos {
                    HalfClosedRemote(local)
                } else {
                    let remote = Data;
                    Open { local, remote }
                };

                Ok(false)
            }
            HalfClosedLocal(remote) => {
                try!(remote.check_is_headers(Reason::ProtocolError));

                *self = if eos {
                    Closed
                } else {
                    HalfClosedLocal(Data)
                };

                Ok(false)
            }
            Closed | HalfClosedRemote(..) => {
                Err(Reason::ProtocolError.into())
            }
            _ => unimplemented!(),
        }
    }

    /// Transition the state to represent headers being sent.
    ///
    /// Returns true if this state transition results in initializing the stream
    /// id. `Err` is returned if this is an invalid state transition.
    pub fn send_headers<P: Peer>(&mut self, eos: bool) -> Result<bool, ConnectionError> {
        use self::State::*;
        use self::PeerState::*;

        match *self {
            Idle => {
                *self = if eos {
                    HalfClosedLocal(Headers)
                } else {
                    Open {
                        local: Data,
                        remote: Headers,
                    }
                };

                Ok(true)
            }
            Open { local, remote } => {
                try!(local.check_is_headers(Reason::InternalError));

                *self = if eos {
                    HalfClosedLocal(remote)
                } else {
                    let local = Data;
                    Open { local, remote }
                };

                Ok(false)
            }
            HalfClosedRemote(local) => {
                try!(local.check_is_headers(Reason::InternalError));

                *self = if eos {
                    Closed
                } else {
                    HalfClosedRemote(Data)
                };

                Ok(false)
            }
            Closed | HalfClosedLocal(..) => {
                Err(Reason::InternalError.into())
            }
            _ => unimplemented!(),
        }
    }
}

impl PeerState {
    #[inline]
    fn check_is_headers(&self, err: Reason) -> Result<(), ConnectionError> {
        use self::PeerState::*;

        match *self {
            Headers => Ok(()),
            _ => Err(err.into()),
        }
    }
}

impl Default for State {
    fn default() -> State {
        State::Idle
    }
}
