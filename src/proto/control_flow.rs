use ConnectionError;
use proto::*;

/// Exposes flow control states to "upper" layers of the transport (i.e. above
/// FlowControl).
pub trait ControlFlow {
    /// Polls for the next window update from the remote.
    fn poll_window_update(&mut self) -> Poll<WindowUpdate, ConnectionError>;

    /// Increases the local receive capacity of a stream.
    ///
    /// This may cause a window update to be sent to the remote.
    ///
    /// Fails if the given stream is not active.
    fn expand_window(&mut self, id: StreamId, incr: WindowSize) -> Result<(), ConnectionError>;
}

macro_rules! proxy_control_flow {
    ($outer:ident) => (
        impl<T: ControlFlow> ControlFlow for $outer<T> {
            fn poll_window_update(&mut self) -> Poll<WindowUpdate, ConnectionError> {
                self.inner.poll_window_update()
            }

            fn expand_window(&mut self, id: StreamId, incr: WindowSize) -> Result<(), ConnectionError> {
                self.inner.expand_window(id, incr)
            }
        }
    )
}
