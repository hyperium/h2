use ConnectionError;
use proto::*;

/// Exposes flow control states to "upper" layers of the transport (i.e. above
/// FlowControl).
pub trait ControlFlowSend {
    /// Polls for the next window update from the remote.
    fn poll_window_update(&mut self) -> Poll<WindowUpdate, ConnectionError>;
}

pub trait ControlFlowRecv {
    /// Increases the local receive capacity of a stream.
    ///
    /// This may cause a window update to be sent to the remote.
    ///
    /// Fails if the given stream is not active.
    fn expand_window(&mut self, id: StreamId, incr: WindowSize) -> Result<(), ConnectionError>;
}

macro_rules! proxy_control_flow_send {
    ($outer:ident) => (
        impl<T: ControlFlowSend> ControlFlowSend for $outer<T> {
            fn poll_window_update(&mut self) -> Poll<WindowUpdate, ConnectionError> {
                self.inner.poll_window_update()
            }
        }
    )
}

macro_rules! proxy_control_flow_recv {
    ($outer:ident) => (
        impl<T: ControlFlowRecv> ControlFlowRecv for $outer<T> {
            fn expand_window(&mut self, id: StreamId, incr: WindowSize) -> Result<(), ConnectionError> {
                self.inner.expand_window(id, incr)
            }
        }
    )
}

macro_rules! proxy_control_flow {
    ($outer:ident) => (
        proxy_control_flow_recv!($outer);
        proxy_control_flow_send!($outer);
    )
}
