use ConnectionError;
use proto::*;

/// Exposes stream states to "upper" layers of the transport (i.e. from StreamTracker up
/// to Connection).
pub trait ControlStreams {
    /// Determines whether the given stream could theoretically be opened by the local
    /// side of this connection.
    fn local_valid_id(id: StreamId) -> bool;

    /// Determines whether the given stream could theoretically be opened by the remote
    /// side of this connection.
    fn remote_valid_id(id: StreamId) -> bool;

    /// Indicates whether this local endpoint may open streams (with HEADERS).
    ///
    /// Implies that this endpoint is a client.
    fn local_can_open() -> bool;

    /// Indicates whether this remote endpoint may open streams (with HEADERS).
    ///
    /// Implies that this endpoint is a server.
    fn remote_can_open() -> bool {
        !Self::local_can_open()
    }

    // TODO push promise
    // fn local_can_reserve(&mut self, id: StreamId) -> Result<(), ConnectionError>;
    // fn remote_can_reserve(&mut self, id: StreamId) -> Result<(), ConnectionError>;

    /// Creates a new stream in the OPEN state from the local side (i.e. as a Client).
    ///
    /// Must only be called when local_can_open returns true.
    fn local_open(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError>;

    /// Create a new stream in the OPEN state from the remote side (i.e. as a Server).
    ///
    /// Must only be called when remote_can_open returns true.
    fn remote_open(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError>;

    /// Prepare the receive side of a local stream to receive data from the remote.
    ///
    /// Typically called when a client receives a response header.
    fn local_open_recv_half(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError>;

    /// Prepare the send side of a remote stream to receive data from the local endpoint.
    ///
    /// Typically called when a server sends a response header.
    fn remote_open_send_half(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError>;

    // TODO push promise
    // fn local_reserve(&mut self, id: StreamId) -> Result<(), ConnectionError>;
    // fn remote_reserve(&mut self, id: StreamId) -> Result<(), ConnectionError>;

    /// Closes the send half of a stream.
    ///
    /// Fails with a ProtocolError if send half of the stream was not open.
    fn close_send_half(&mut self, id: StreamId) -> Result<(), ConnectionError>;

    /// Closes the recv half of a stream.
    ///
    /// Fails with a ProtocolError if recv half of the stream was not open.
    fn close_recv_half(&mut self, id: StreamId) -> Result<(), ConnectionError>;

    /// Resets the given stream.
    ///
    /// If the stream was already reset, the stored cause is updated.
    fn reset_stream(&mut self, id: StreamId, cause: Reason);

    /// Get the reason the stream was reset, if it was reset.
    fn get_reset(&self, id: StreamId) -> Option<Reason>;

    /// Returns true if the given stream was opened by the local peer and is not yet
    /// closed.
    fn is_local_active(&self, id: StreamId) -> bool;

    /// Returns true if the given stream was opened by the remote peer and is not yet
    /// closed.
    fn is_remote_active(&self, id: StreamId) -> bool;

    /// Returns true if the given stream was opened and is not yet closed.
    fn is_active(&self, id: StreamId) -> bool {
        if Self::local_valid_id(id) {
            self.is_local_active(id)
        } else {
            self.is_remote_active(id)
        }
    }

    /// Returns the number of open streams initiated by the local peer.
    fn local_active_len(&self) -> usize;

    /// Returns the number of open streams initiated by the remote peer.
    fn remote_active_len(&self) -> usize;

    /// Returns true iff the recv half of the given stream is open.
    fn is_recv_open(&mut self, id: StreamId) -> bool;

    /// Returns true iff the send half of the given stream is open.
    fn is_send_open(&mut self, id: StreamId) -> bool;

    /// If the given stream ID is active and able to recv data, get its mutable recv flow
    /// control state.
    fn recv_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState>;

    /// If the given stream ID is active and able to send data, get its mutable send flow
    /// control state.
    fn send_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState>;

    /// Updates the initial window size for the local peer.
    fn update_inital_recv_window_size(&mut self, old_sz: WindowSize, new_sz: WindowSize);

    /// Updates the initial window size for the remote peer.
    fn update_inital_send_window_size(&mut self, old_sz: WindowSize, new_sz: WindowSize);
}

macro_rules! proxy_control_streams {
    ($outer:ident) => (
        impl<T: ControlStreams> ControlStreams for $outer<T> {
            fn local_valid_id(id: StreamId) -> bool {
                T::local_valid_id(id)
            }

            fn remote_valid_id(id: StreamId) -> bool {
                T::remote_valid_id(id)
            }

            fn local_can_open() -> bool {
                T::local_can_open()
            }

            fn local_open(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError> {
                self.inner.local_open(id, sz)
            }

            fn remote_open(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError> {
                self.inner.remote_open(id, sz)
            }

            fn local_open_recv_half(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError> {
                self.inner.local_open_recv_half(id, sz)
            }

            fn remote_open_send_half(&mut self, id: StreamId, sz: WindowSize) -> Result<(), ConnectionError> {
                self.inner.remote_open_send_half(id, sz)
            }

            fn close_send_half(&mut self, id: StreamId) -> Result<(), ConnectionError> {
                self.inner.close_send_half(id)
            }

            fn close_recv_half(&mut self, id: StreamId) -> Result<(), ConnectionError> {
                self.inner.close_recv_half(id)
            }

            fn reset_stream(&mut self, id: StreamId, cause: Reason) {
                self.inner.reset_stream(id, cause)
            }

            fn get_reset(&self, id: StreamId) -> Option<Reason> {
                self.inner.get_reset(id)
            }

            fn is_local_active(&self, id: StreamId) -> bool {
                self.inner.is_local_active(id)
            }

            fn is_remote_active(&self, id: StreamId) -> bool {
                self.inner.is_remote_active(id)
            }

            fn local_active_len(&self) -> usize {
                self.inner.local_active_len()
            }

            fn remote_active_len(&self) -> usize {
                self.inner.remote_active_len()
            }

            fn update_inital_recv_window_size(&mut self, old_sz: WindowSize, new_sz: WindowSize) {
                self.inner.update_inital_recv_window_size(old_sz, new_sz)
            }

            fn update_inital_send_window_size(&mut self, old_sz: WindowSize, new_sz: WindowSize) {
                self.inner.update_inital_send_window_size(old_sz, new_sz)
            }

            fn recv_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState> {
                self.inner.recv_flow_controller(id)
            }

            fn send_flow_controller(&mut self, id: StreamId) -> Option<&mut FlowControlState> {
                self.inner.send_flow_controller(id)
            }

            fn is_send_open(&mut self, id: StreamId) -> bool {
                self.inner.is_send_open(id)
            }

            fn is_recv_open(&mut self, id: StreamId) -> bool  {
                self.inner.is_recv_open(id)
            }
        }
    )
}
