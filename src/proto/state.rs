use ConnectionError;
use error::Reason;
use error::Reason::*;
use error::User::*;
use proto::*;

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
pub enum Stream {
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
pub enum Peer {
    AwaitingHeaders,
    /// Contains a FlowControl representing the _receiver_ of this this data stream.
    Streaming(FlowControl),
}

#[derive(Copy, Clone, Debug)]
pub struct FlowControl {
    /// Amount that may be claimed.
    window_size: WindowSize,

    /// Amount to be removed by future increments.
    underflow: WindowSize,

    /// The amount that has been incremented but not yet advertised (to the application or
    /// the remote).
    next_window_update: WindowSize,
}

impl Stream {
    /// Opens the send-half of a stream if it is not already open.
    pub fn send_open(&mut self, sz: WindowSize, eos: bool) -> Result<(), ConnectionError> {
        use self::Stream::*;
        use self::Peer::*;

        let local = Peer::streaming(sz);

        *self = match *self {
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
        use self::Stream::*;
        use self::Peer::*;

        let remote = Peer::streaming(sz);

        *self = match *self {
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
        use self::Stream::*;

        match *self {
            Open { local, .. } => {
                // The remote side will continue to receive data.
                trace!("recv_close: Open => HalfClosedRemote({:?})", local);
                *self = HalfClosedRemote(local);
                Ok(())
            }
            HalfClosedLocal(..) => {
                trace!("recv_close: HalfClosedLocal => Closed");
                *self = Closed(None);
                Ok(())
            }
            _ => Err(ProtocolError.into()),
        }
    }

    /// Indicates that the local side will not send more data to the local.
    pub fn send_close(&mut self) -> Result<(), ConnectionError> {
        use self::Stream::*;

        match *self {
            Open { remote, .. } => {
                // The remote side will continue to receive data.
                trace!("send_close: Open => HalfClosedLocal({:?})", remote);
                *self = HalfClosedLocal(remote);
                Ok(())
            }
            HalfClosedRemote(..) => {
                trace!("send_close: HalfClosedRemote => Closed");
                *self = Closed(None);
                Ok(())
            }
            _ => Err(ProtocolError.into()),
        }
    }

    pub fn is_closed(&self) -> bool {
        use self::Stream::*;

        match *self {
            Closed(_) => true,
            _ => false,
        }
    }

    pub fn recv_flow_control(&mut self) -> Option<&mut FlowControl> {
        use self::Stream::*;

        match *self {
            Open { ref mut remote, .. } |
            HalfClosedLocal(ref mut remote) => remote.flow_control(),
            _ => None,
        }
    }

    pub fn send_flow_control(&mut self) -> Option<&mut FlowControl> {
        use self::Stream::*;

        match *self {
            Open { ref mut local, .. } |
            HalfClosedRemote(ref mut local) => local.flow_control(),
            _ => None,
        }
    }
}

impl Default for Stream {
    fn default() -> Stream {
        Stream::Idle
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
        use self::Peer::*;

        match *self {
            Streaming(ref mut flow) => Some(flow),
            _ => None,
        }
    }
}

impl FlowControl {
    pub fn new(window_size: WindowSize) -> FlowControl {
        FlowControl {
            window_size,
            underflow: 0,
            next_window_update: 0,
        }
    }

    /// Returns true iff `claim_window(sz)` would return succeed.
    pub fn ensure_window<T>(&mut self, sz: WindowSize, err: T) -> Result<(), ConnectionError>
        where T: Into<ConnectionError>,
    {
        if sz <= self.window_size {
            Ok(())
        } else {
            Err(err.into())
        }
    }

    /// Claims the provided amount from the window, if there is enough space.
    ///
    /// Fails when `apply_window_update()` hasn't returned at least `sz` more bytes than
    /// have been previously claimed.
    pub fn claim_window<T>(&mut self, sz: WindowSize, err: T)
        -> Result<(), ConnectionError>
        where T: Into<ConnectionError>,
    {
        self.ensure_window(sz, err)?;

        self.window_size -= sz;
        Ok(())
    }

    /// Increase the _unadvertised_ window capacity.
    pub fn expand_window(&mut self, sz: WindowSize) {
        if sz <= self.underflow {
            self.underflow -= sz;
            return;
        }

        let added = sz - self.underflow;
        self.next_window_update += added;
        self.underflow = 0;
    }

    /// Obtains the unadvertised window update.
    ///
    /// This does not apply the window update to `self`.
    pub fn peek_window_update(&mut self) -> Option<WindowSize> {
        if self.next_window_update == 0 {
            None
        } else {
            Some(self.next_window_update)
        }
    }

    /// Obtains and applies an unadvertised window update.
    pub fn apply_window_update(&mut self) -> Option<WindowSize> {
        if self.next_window_update == 0 {
            return None;
        }

        let incr = self.next_window_update;
        self.next_window_update = 0;
        self.window_size += incr;
        Some(incr)
    }
}
