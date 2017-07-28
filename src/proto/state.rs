use ConnectionError;
use error::Reason;
use error::Reason::*;
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
    window_size: usize,

    /// Amount to be removed by future increments.
    underflow: usize,

    /// The amount that has been incremented but not yet advertised (to the application or
    /// the remote).
    next_window_update: usize,
}

impl Stream {
    /// Opens the send-half of a stream if it is not already open.
    ///
    /// Returns true iff the send half was not previously open.
    pub fn send_open(&mut self, sz: usize) -> Result<bool, ConnectionError> {
        unimplemented!();
        /*
        use self::Stream::*;
        use self::Peer::*;

        // Try to avoid copying `self` by first checking to see whether the stream needs
        // to be updated.
        match self {
            &mut Idle |
            &mut Closed(_) |
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
                    remote: Peer::streaming(sz),
                };
            }

            HalfClosedLocal(AwaitingHeaders) => {
                *self = HalfClosedLocal(Peer::streaming(sz));
            }

            _ => unreachable!()
        }

        Ok(true)
        */
    }

    /// Open the receive have of the stream, this action is taken when a HEADERS
    /// frame is received.
    pub fn recv_open(&mut self, sz: usize, eos: bool) -> Result<(), ConnectionError> {
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
                        remote: remote,
                    }
                }
            }
            Open { local, remote: AwaitingHeaders } => {
                if eos {
                    HalfClosedRemote(local)
                } else {
                    Open {
                        local,
                        remote: remote,
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

    pub fn is_closed(&self) -> bool {
        use self::Stream::*;

        match *self {
            Closed(_) => true,
            _ => false,
        }
    }

    /*
    pub fn is_send_open(&self) -> bool {
        use self::Stream::*;

        match self {
            &Idle | &Closed(_) | &HalfClosedRemote(..) => false,

            &Open { ref remote, .. } |
            &HalfClosedLocal(ref remote) => remote.is_streaming(),
        }
    }

    pub fn is_recv_open(&self) -> bool {
        use self::Stream::*;

        match self {
            &Idle | &Closed(_) | &HalfClosedLocal(..) => false,

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
        use self::Stream::*;

        match *self {
            Open { local, .. } => {
                // The local side will continue to receive data.
                trace!("close_send_half: Open => HalfClosedRemote({:?})", local);
                *self = HalfClosedRemote(local);
                Ok(false)
            }

            HalfClosedLocal(..) => {
                trace!("close_send_half: HalfClosedLocal => Closed");
                *self = Closed(None);
                Ok(true)
            }

            Idle | Closed(_) | HalfClosedRemote(..) => {
                Err(ProtocolError.into())
            }
        }
    }

    /// Indicates that the remote side will not send more data to the local.
    ///
    /// Returns true iff the stream is fully closed.
    pub fn close_recv_half(&mut self) -> Result<bool, ConnectionError> {
        use self::Stream::*;

        match *self {
            Open { remote, .. } => {
                // The remote side will continue to receive data.
                trace!("close_recv_half: Open => HalfClosedLocal({:?})", remote);
                *self = HalfClosedLocal(remote);
                Ok(false)
            }

            HalfClosedRemote(..) => {
                trace!("close_recv_half: HalfClosedRemoteOpen => Closed");
                *self = Closed(None);
                Ok(true)
            }

            Idle | Closed(_) | HalfClosedLocal(..) => {
                Err(ProtocolError.into())
            }
        }
    }
    */

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
    fn streaming(sz: usize) -> Peer {
        Peer::Streaming(FlowControl::new(sz))
    }

    fn is_streaming(&self) -> bool {
        use self::Peer::*;

        match self {
            &Streaming(..) => true,
            _ => false,
        }
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
    pub fn new(window_size: usize) -> FlowControl {
        FlowControl {
            window_size,
            underflow: 0,
            next_window_update: 0,
        }
    }

    /// Reduce future capacity of the window.
    ///
    /// This accomodates updates to SETTINGS_INITIAL_WINDOW_SIZE.
    pub fn shrink_window(&mut self, decr: usize) {
        if decr < self.next_window_update {
            self.next_window_update -= decr
        } else {
            self.underflow += decr - self.next_window_update;
            self.next_window_update = 0;
        }
    }

    /// Returns true iff `claim_window(sz)` would return succeed.
    pub fn ensure_window(&mut self, sz: usize) -> Result<(), ConnectionError> {
        if sz <= self.window_size {
            Ok(())
        } else {
            Err(FlowControlError.into())
        }
    }

    /// Claims the provided amount from the window, if there is enough space.
    ///
    /// Fails when `apply_window_update()` hasn't returned at least `sz` more bytes than
    /// have been previously claimed.
    pub fn claim_window(&mut self, sz: usize) -> Result<(), ConnectionError> {
        try!(self.ensure_window(sz));

        self.window_size -= sz;
        Ok(())
    }

    /// Increase the _unadvertised_ window capacity.
    pub fn expand_window(&mut self, sz: usize) {
        if sz <= self.underflow {
            self.underflow -= sz;
            return;
        }

        let added = sz - self.underflow;
        self.next_window_update += added;
        self.underflow = 0;
    }

    /// Obtains and applies an unadvertised window update.
    pub fn apply_window_update(&mut self) -> Option<usize> {
        if self.next_window_update == 0 {
            return None;
        }

        let incr = self.next_window_update;
        self.next_window_update = 0;
        self.window_size += incr;
        Some(incr)
    }
}
