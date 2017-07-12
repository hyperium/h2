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

impl State {
    /// Updates the local flow controller with the given window size increment.
    ///
    /// Returns the amount of capacity created, accounting for window size changes. The
    /// caller should send the the returned window size increment to the remote.
    ///
    /// If the remote is closed, None is returned.
    pub fn send_window_update(&mut self, incr: u32) -> Option<u32> {
        use self::State::*;
        use self::PeerState::*;

        if incr == 0 {
            return None;
        }

        match self {
            &mut Open { local: Data(ref mut fc), .. } |
            &mut HalfClosedRemote(Data(ref mut fc)) => {
                fc.add_to_window(incr);
                fc.take_window_update()
            }
            _ => None,
        }
    }
 
    /// Updates the remote flow controller with the given window size increment.
    ///
    /// Returns the amount of capacity created, accounting for window size changes. The
    /// caller should send the the returned window size increment to the remote.
    pub fn recv_window_update(&mut self, incr: u32) {
        use self::State::*;
        use self::PeerState::*;

        if incr == 0 {
            return;
        }

        match self {
            &mut Open { remote: Data(ref mut fc), .. } |
            &mut HalfClosedLocal(Data(ref mut fc)) => fc.add_to_window(incr),
            _ => {},
        }
    }

    pub fn take_remote_window_update(&mut self) -> Option<u32> {
        use self::State::*;
        use self::PeerState::*;

        match self {
            &mut Open { remote: Data(ref mut fc), .. } |
            &mut HalfClosedLocal(Data(ref mut fc)) => fc.take_window_update(),
            _ => None,
        }
    }

    /// Applies an update to the remote's initial window size.
    ///
    /// Per RFC 7540 §6.9.2
    ///
    /// > In addition to changing the flow-control window for streams that are not yet
    /// > active, a SETTINGS frame can alter the initial flow-control window size for
    /// > streams with active flow-control windows (that is, streams in the "open" or
    /// > "half-closed (remote)" state). When the value of SETTINGS_INITIAL_WINDOW_SIZE
    /// > changes, a receiver MUST adjust the size of all stream flow-control windows that
    /// > it maintains by the difference between the new value and the old value.
    /// >
    /// > A change to `SETTINGS_INITIAL_WINDOW_SIZE` can cause the available space in a
    /// > flow-control window to become negative. A sender MUST track the negative
    /// > flow-control window and MUST NOT send new flow-controlled frames until it
    /// > receives WINDOW_UPDATE frames that cause the flow-control window to become
    /// > positive.
    pub fn update_remote_initial_window_size(&mut self, old: u32, new: u32) {
        use self::State::*;
        use self::PeerState::*;

        match self {
            &mut Open { remote: Data(ref mut fc), .. } |
            &mut HalfClosedLocal(Data(ref mut fc)) => {
                if new < old {
                    fc.shrink_window(old - new);
                } else {
                    fc.add_to_window(new - old);
                }
            }
            _ => {}
        }
    }

    /// TODO Connection doesn't have an API for local updates yet.
    pub fn update_local_initial_window_size(&mut self, _old: u32, _new: u32) {
        //use self::State::*;
        //use self::PeerState::*;
        unimplemented!()
    }

    /// Transition the state to represent headers being received.
    ///
    /// Returns true if this state transition results in iniitializing the
    /// stream id. `Err` is returned if this is an invalid state transition.
    pub fn recv_headers<P: Peer>(&mut self,
                                 eos: bool,
                                 remote_window_size: u32)
        -> Result<bool, ConnectionError>
    {
        use self::State::*;
        use self::PeerState::*;

        match *self {
            Idle => {
                let local = Headers;
                if eos {
                    *self = HalfClosedRemote(local);
                } else {
                    *self = Open { local, remote: Data(FlowController::new(remote_window_size)) };
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
                    *self = HalfClosedLocal(Data(FlowController::new(remote_window_size)));
                };
                Ok(false)
            }

            Closed | HalfClosedRemote(..) => {
                Err(ProtocolError.into())
            }

            _ => unimplemented!(),
        }
    }

    pub fn recv_data(&mut self, eos: bool, len: usize) -> Result<(), ConnectionError> {
        use self::State::*;

        match *self {
            Open { local, remote } => {
                try!(remote.check_is_data(ProtocolError.into()));
                try!(remote.check_window_size(len, FlowControlError.into()));
                if eos {
                    *self = HalfClosedRemote(local);
                }
                Ok(())
            }

            HalfClosedLocal(remote) => {
                try!(remote.check_is_data(ProtocolError.into()));
                try!(remote.check_window_size(len, FlowControlError.into()));
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

    pub fn send_data(&mut self, eos: bool, len: usize) -> Result<(), ConnectionError> {
        use self::State::*;

        match *self {
            Open { local, remote } => {
                try!(local.check_is_data(UnexpectedFrameType.into()));
                try!(local.check_window_size(len, FlowControlViolation.into()));
                if eos {
                    *self = HalfClosedLocal(remote);
                }
                Ok(())
            }

            HalfClosedRemote(local) => {
                try!(local.check_is_data(UnexpectedFrameType.into()));
                try!(local.check_window_size(len, FlowControlViolation.into()));
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

impl Default for State {
    fn default() -> State {
        State::Idle
    }
}

#[derive(Debug, Copy, Clone)]
pub enum PeerState {
    Headers,
    /// Contains a FlowController representing the _receiver_ of this this data stream.
    Data(FlowController),
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

    #[inline]
    fn check_window_size(&self, len: usize, err: ConnectionError) -> Result<(), ConnectionError> {
        use self::PeerState::*;
        match self {
            &Data(ref fc) if len <= fc.window_size() as usize=> Ok(()),
            _ => Err(err),
        }
    }
}
