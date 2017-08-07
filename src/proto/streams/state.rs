use ConnectionError;
use error::Reason;
use error::Reason::*;
use error::User::*;
use proto::*;
use super::FlowControl;

use self::Inner::*;
use self::Peer::*;

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
#[derive(Debug, Clone)]
pub struct State {
    inner: Inner,
}

#[derive(Debug, Clone, Copy)]
enum Inner {
    Idle,
    // TODO: these states shouldn't count against concurrency limits:
    //ReservedLocal,
    //ReservedRemote,
    Open {
        local: Peer,
        remote: Peer,
    },
    HalfClosedLocal(Peer), // TODO: explicitly name this value
    HalfClosedRemote(Peer),
    // When reset, a reason is provided
    Closed(Option<Reason>),
}

#[derive(Debug, Copy, Clone)]
enum Peer {
    AwaitingHeaders,
    /// Contains a FlowControl representing the _receiver_ of this this data stream.
    Streaming(FlowControl),
}

impl State {
    /// Opens the send-half of a stream if it is not already open.
    pub fn send_open(&mut self, sz: WindowSize, eos: bool) -> Result<(), ConnectionError> {
        let local = Peer::streaming(sz);

        self.inner = match self.inner {
            Idle => {
                if eos {
                    HalfClosedLocal(AwaitingHeaders)
                } else {
                    Open {
                        local,
                        remote: AwaitingHeaders,
                    }
                }
            }
            Open { local: AwaitingHeaders, remote } => {
                if eos {
                    HalfClosedLocal(remote)
                } else {
                    Open {
                        local,
                        remote,
                    }
                }
            }
            HalfClosedRemote(AwaitingHeaders) => {
                if eos {
                    Closed(None)
                } else {
                    HalfClosedRemote(local)
                }
            }
            _ => {
                // All other transitions result in a protocol error
                return Err(UnexpectedFrameType.into());
            }
        };

        return Ok(());
    }

    /// Open the receive have of the stream, this action is taken when a HEADERS
    /// frame is received.
    pub fn recv_open(&mut self, sz: WindowSize, eos: bool) -> Result<(), ConnectionError> {
        let remote = Peer::streaming(sz);

        self.inner = match self.inner {
            Idle => {
                if eos {
                    HalfClosedRemote(AwaitingHeaders)
                } else {
                    Open {
                        local: AwaitingHeaders,
                        remote,
                    }
                }
            }
            Open { local, remote: AwaitingHeaders } => {
                if eos {
                    HalfClosedRemote(local)
                } else {
                    Open {
                        local,
                        remote,
                    }
                }
            }
            HalfClosedLocal(AwaitingHeaders) => {
                if eos {
                    Closed(None)
                } else {
                    HalfClosedLocal(remote)
                }
            }
            _ => {
                // All other transitions result in a protocol error
                return Err(ProtocolError.into());
            }
        };

        return Ok(());
    }

    /// Indicates that the remote side will not send more data to the local.
    pub fn recv_close(&mut self) -> Result<(), ConnectionError> {
        match self.inner {
            Open { local, .. } => {
                // The remote side will continue to receive data.
                trace!("recv_close: Open => HalfClosedRemote({:?})", local);
                self.inner = HalfClosedRemote(local);
                Ok(())
            }
            HalfClosedLocal(..) => {
                trace!("recv_close: HalfClosedLocal => Closed");
                self.inner = Closed(None);
                Ok(())
            }
            _ => Err(ProtocolError.into()),
        }
    }

    /// Indicates that the local side will not send more data to the local.
    pub fn send_close(&mut self) -> Result<(), ConnectionError> {
        match self.inner {
            Open { remote, .. } => {
                // The remote side will continue to receive data.
                trace!("send_close: Open => HalfClosedLocal({:?})", remote);
                self.inner = HalfClosedLocal(remote);
                Ok(())
            }
            HalfClosedRemote(..) => {
                trace!("send_close: HalfClosedRemote => Closed");
                self.inner = Closed(None);
                Ok(())
            }
            _ => Err(ProtocolError.into()),
        }
    }

    pub fn is_closed(&self) -> bool {
        match self.inner {
            Closed(_) => true,
            _ => false,
        }
    }

    pub fn is_recv_closed(&self) -> bool {
        match self.inner {
            Closed(..) | HalfClosedRemote(..) => true,
            _ => false,
        }
    }

    pub fn recv_flow_control(&mut self) -> Option<&mut FlowControl> {
        match self.inner {
            Open { ref mut remote, .. } |
            HalfClosedLocal(ref mut remote) => remote.flow_control(),
            _ => None,
        }
    }

    pub fn send_flow_control(&mut self) -> Option<&mut FlowControl> {
        match self.inner {
            Open { ref mut local, .. } |
            HalfClosedRemote(ref mut local) => local.flow_control(),
            _ => None,
        }
    }
}

impl Default for State {
    fn default() -> State {
        State { inner: Inner::Idle }
    }
}

impl Default for Peer {
    fn default() -> Self {
        Peer::AwaitingHeaders
    }
}

impl Peer {
    fn streaming(sz: WindowSize) -> Peer {
        Peer::Streaming(FlowControl::new(sz))
    }

    fn flow_control(&mut self) -> Option<&mut FlowControl> {
        match *self {
            Streaming(ref mut flow) => Some(flow),
            _ => None,
        }
    }
}
