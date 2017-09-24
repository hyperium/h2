use codec::{RecvError, UserError};
use codec::UserError::*;
use frame::Reason;
use frame::Reason::*;
use proto;

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
    ReservedRemote,
    Open { local: Peer, remote: Peer },
    HalfClosedLocal(Peer), // TODO: explicitly name this value
    HalfClosedRemote(Peer),
    // When reset, a reason is provided
    Closed(Option<Cause>),
}

#[derive(Debug, Copy, Clone)]
enum Peer {
    AwaitingHeaders,
    Streaming,
}

#[derive(Debug, Copy, Clone)]
enum Cause {
    Proto(Reason),
    Io,
}

impl State {
    /// Opens the send-half of a stream if it is not already open.
    pub fn send_open(&mut self, eos: bool) -> Result<(), UserError> {
        let local = Peer::Streaming;

        self.inner = match self.inner {
            Idle => if eos {
                HalfClosedLocal(AwaitingHeaders)
            } else {
                Open {
                    local,
                    remote: AwaitingHeaders,
                }
            },
            Open {
                local: AwaitingHeaders,
                remote,
            } => if eos {
                HalfClosedLocal(remote)
            } else {
                Open {
                    local,
                    remote,
                }
            },
            HalfClosedRemote(AwaitingHeaders) => if eos {
                Closed(None)
            } else {
                HalfClosedRemote(local)
            },
            _ => {
                // All other transitions result in a protocol error
                return Err(UnexpectedFrameType);
            },
        };

        return Ok(());
    }

    /// Opens the receive-half of the stream when a HEADERS frame is received.
    ///
    /// Returns true if this transitions the state to Open.
    pub fn recv_open(&mut self, eos: bool) -> Result<bool, RecvError> {
        let remote = Peer::Streaming;
        let mut initial = false;

        self.inner = match self.inner {
            Idle => {
                initial = true;

                if eos {
                    HalfClosedRemote(AwaitingHeaders)
                } else {
                    Open {
                        local: AwaitingHeaders,
                        remote,
                    }
                }
            },
            ReservedRemote => {
                initial = true;

                if eos {
                    Closed(None)
                } else {
                    Open {
                        local: AwaitingHeaders,
                        remote,
                    }
                }
            },
            Open {
                local,
                remote: AwaitingHeaders,
            } => if eos {
                HalfClosedRemote(local)
            } else {
                Open {
                    local,
                    remote,
                }
            },
            HalfClosedLocal(AwaitingHeaders) => if eos {
                Closed(None)
            } else {
                HalfClosedLocal(remote)
            },
            _ => {
                // All other transitions result in a protocol error
                return Err(RecvError::Connection(ProtocolError));
            },
        };

        return Ok(initial);
    }

    /// Transition from Idle -> ReservedRemote
    pub fn reserve_remote(&mut self) -> Result<(), RecvError> {
        match self.inner {
            Idle => {
                self.inner = ReservedRemote;
                Ok(())
            },
            _ => Err(RecvError::Connection(ProtocolError)),
        }
    }

    /// Indicates that the remote side will not send more data to the local.
    pub fn recv_close(&mut self) -> Result<(), RecvError> {
        match self.inner {
            Open {
                local, ..
            } => {
                // The remote side will continue to receive data.
                trace!("recv_close: Open => HalfClosedRemote({:?})", local);
                self.inner = HalfClosedRemote(local);
                Ok(())
            },
            HalfClosedLocal(..) => {
                trace!("recv_close: HalfClosedLocal => Closed");
                self.inner = Closed(None);
                Ok(())
            },
            _ => Err(RecvError::Connection(ProtocolError)),
        }
    }

    pub fn recv_err(&mut self, err: &proto::Error) {
        use proto::Error::*;

        match self.inner {
            Closed(..) => {},
            _ => {
                trace!("recv_err; err={:?}", err);
                self.inner = Closed(match *err {
                    Proto(reason) => Some(Cause::Proto(reason)),
                    Io(..) => Some(Cause::Io),
                });
            },
        }
    }

    /// Indicates that the local side will not send more data to the local.
    pub fn send_close(&mut self) {
        match self.inner {
            Open {
                remote, ..
            } => {
                // The remote side will continue to receive data.
                trace!("send_close: Open => HalfClosedLocal({:?})", remote);
                self.inner = HalfClosedLocal(remote);
            },
            HalfClosedRemote(..) => {
                trace!("send_close: HalfClosedRemote => Closed");
                self.inner = Closed(None);
            },
            _ => panic!("transition send_close on unexpected state"),
        }
    }

    /// Set the stream state to reset
    pub fn set_reset(&mut self, reason: Reason) {
        debug_assert!(!self.is_reset());
        self.inner = Closed(Some(Cause::Proto(reason)));
    }

    /// Returns true if the stream is already reset.
    pub fn is_reset(&self) -> bool {
        match self.inner {
            Closed(Some(_)) => true,
            _ => false,
        }
    }

    /// Returns true if a stream with the current state counts against the
    /// concurrency limit.
    pub fn is_counted(&self) -> bool {
        match self.inner {
            Open {
                ..
            } => true,
            HalfClosedLocal(..) => true,
            HalfClosedRemote(..) => true,
            _ => false,
        }
    }

    pub fn is_send_streaming(&self) -> bool {
        match self.inner {
            Open {
                local: Peer::Streaming,
                ..
            } => true,
            HalfClosedRemote(Peer::Streaming) => true,
            _ => false,
        }
    }

    /// Returns true when the stream is in a state to receive headers
    pub fn is_recv_headers(&self) -> bool {
        match self.inner {
            Idle => true,
            Open {
                remote: AwaitingHeaders,
                ..
            } => true,
            HalfClosedLocal(AwaitingHeaders) => true,
            ReservedRemote => true,
            _ => false,
        }
    }

    pub fn is_recv_streaming(&self) -> bool {
        match self.inner {
            Open {
                remote: Peer::Streaming,
                ..
            } => true,
            HalfClosedLocal(Peer::Streaming) => true,
            _ => false,
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

    pub fn ensure_recv_open(&self) -> Result<bool, proto::Error> {
        use std::io;

        // TODO: Is this correct?
        match self.inner {
            Closed(Some(Cause::Proto(reason))) => Err(proto::Error::Proto(reason)),
            Closed(Some(Cause::Io)) => Err(proto::Error::Io(io::ErrorKind::BrokenPipe.into())),
            Closed(None) | HalfClosedRemote(..) => Ok(false),
            _ => Ok(true),
        }
    }
}

impl Default for State {
    fn default() -> State {
        State {
            inner: Inner::Idle,
        }
    }
}

impl Default for Peer {
    fn default() -> Self {
        Peer::AwaitingHeaders
    }
}
