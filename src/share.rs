use codec::UserError;
use frame::Reason;
use proto::{self, WindowSize};

use bytes::{Bytes, IntoBuf};
use futures::{self, Poll, Async};
use http::{HeaderMap};

use std::fmt;

/// Send the body stream and trailers to the peer.
#[derive(Debug)]
pub struct SendStream<B: IntoBuf> {
    inner: proto::StreamRef<B::Buf>,
}

/// Receive the body stream and trailers from the peer.
#[must_use = "streams do nothing unless polled"]
pub struct RecvStream {
    inner: ReleaseCapacity,
}

/// A handle to release window capacity to a remote stream.
///
/// This type allows the caller to manage inbound data [flow control]. The
/// caller is expected to call [`release_capacity`] after dropping data frames.
///
/// # Overview
///
/// Each stream has a window size. This window size is the maximum amount of
/// inbound data that can be in-flight. In-flight data is defined as data that
/// has been received, but not yet released.
///
/// When a stream is created, the window size is set to the connection's initial
/// window size value. When a data frame is received, the window size is then
/// decremented by size of the data frame before the data is provided to the
/// caller. As the caller finishes using the data, [`release_capacity`] must be
/// called. This will then increment the window size again, allowing the peer to
/// send more data.
///
/// There is also a connection level window as well as the stream level window.
/// Received data counts against the connection level window as well and calls
/// to [`release_capacity`] will also increment the connection level window.
///
/// # Sending `WINDOW_UPDATE` frames
///
/// `WINDOW_UPDATE` frames will not be sent out for **every** call to
/// `release_capacity`, as this would end up slowing down the protocol. Instead,
/// `h2` waits until the window size is increased to a certain threshold and
/// then sends out a single `WINDOW_UPDATE` frame representing all the calls to
/// `release_capacity` since the last `WINDOW_UPDATE` frame.
///
/// This essentially batches window updating.
///
/// # Scenarios
///
/// Following is a basic scenario with an HTTP/2.0 connection containing a
/// single active stream.
///
/// * A new stream is activated. The receive window is initialized to 1024 (the
///   value of the initial window size for this connection).
/// * A `DATA` frame is received containing a payload of 400 bytes.
/// * The receive window size is reduced to 424 bytes.
/// * [`release_capacity`] is called with 200.
/// * The receive window size is now 624 bytes. The peer may send no more than
///   this.
/// * A `DATA` frame is received with a payload of 624 bytes.
/// * The window size is now 0 bytes. The peer may not send any more data.
/// * [`release_capacity`] is called with 1024.
/// * The receive window size is now 1024 bytes. The peer may now send more
/// data.
///
/// [flow control]: ../index.html#flow-control
/// [`release_capacity`]: struct.ReleaseCapacity.html#method.release_capacity
#[derive(Debug)]
pub struct ReleaseCapacity {
    inner: proto::OpaqueStreamRef,
}

// ===== impl SendStream =====

impl<B: IntoBuf> SendStream<B> {
    pub(crate) fn new(inner: proto::StreamRef<B::Buf>) -> Self {
        SendStream { inner }
    }

    /// Request capacity to send data
    pub fn reserve_capacity(&mut self, capacity: usize) {
        // TODO: Check for overflow
        self.inner.reserve_capacity(capacity as WindowSize)
    }

    /// Returns the stream's current send capacity.
    pub fn capacity(&self) -> usize {
        self.inner.capacity() as usize
    }

    /// Request to be notified when the stream's capacity increases
    pub fn poll_capacity(&mut self) -> Poll<Option<usize>, ::Error> {
        let res = try_ready!(self.inner.poll_capacity());
        Ok(Async::Ready(res.map(|v| v as usize)))
    }

    /// Send a single data frame
    pub fn send_data(&mut self, data: B, end_of_stream: bool) -> Result<(), ::Error> {
        self.inner
            .send_data(data.into_buf(), end_of_stream)
            .map_err(Into::into)
    }

    /// Send trailers
    pub fn send_trailers(&mut self, trailers: HeaderMap) -> Result<(), ::Error> {
        self.inner.send_trailers(trailers).map_err(Into::into)
    }

    /// Reset the stream
    pub fn send_reset(&mut self, reason: Reason) {
        self.inner.send_reset(reason)
    }
}

// ===== impl RecvStream =====

impl RecvStream {
    pub(crate) fn new(inner: ReleaseCapacity) -> Self {
        RecvStream { inner }
    }

    #[deprecated(since = "0.0.0")]
    #[doc(hidden)]
    pub fn is_empty(&self) -> bool {
        // If the recv side is closed and the receive queue is empty, the body is empty.
        self.inner.inner.body_is_empty()
    }

    /// Returns true if the receive half has reached the end of stream.
    ///
    /// A return value of `true` means that calls to `poll` and `poll_trailers`
    /// will both return `None`.
    pub fn is_end_stream(&self) -> bool {
        self.inner.inner.is_end_stream()
    }

    /// Get a mutable reference to this streams `ReleaseCapacity`.
    ///
    /// It can be used immediately, or cloned to be used later.
    pub fn release_capacity(&mut self) -> &mut ReleaseCapacity {
        &mut self.inner
    }

    /// Returns received trailers.
    pub fn poll_trailers(&mut self) -> Poll<Option<HeaderMap>, ::Error> {
        self.inner.inner.poll_trailers().map_err(Into::into)
    }
}

impl futures::Stream for RecvStream {
    type Item = Bytes;
    type Error = ::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        self.inner.inner.poll_data().map_err(Into::into)
    }
}

impl fmt::Debug for RecvStream {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("RecvStream")
            .field("inner", &self.inner)
            .finish()
    }
}

// ===== impl ReleaseCapacity =====

impl ReleaseCapacity {
    pub(crate) fn new(inner: proto::OpaqueStreamRef) -> Self {
        ReleaseCapacity { inner }
    }

    /// Release window capacity back to remote stream.
    ///
    /// This releases capacity back to the stream level and the connection level
    /// windows. Both window sizes will be increased by `sz`.
    ///
    /// See [struct level] documentation for more details.
    ///
    /// # Panics
    ///
    /// This function panics if increasing the receive window size by `sz` would
    /// result in a window size greater than the target window size set by
    /// [`set_target_window_size`]. In other words, the caller cannot release
    /// more capacity than data has been received. If 1024 bytes of data have
    /// been received, at most 1024 bytes can be released.
    ///
    /// [struct level]: #
    /// [`set_target_window_size`]: server/struct.Server.html#method.set_target_window_size
    pub fn release_capacity(&mut self, sz: usize) -> Result<(), ::Error> {
        if sz > proto::MAX_WINDOW_SIZE as usize {
            return Err(UserError::ReleaseCapacityTooBig.into());
        }
        self.inner
            .release_capacity(sz as proto::WindowSize)
            .map_err(Into::into)
    }
}

impl Clone for ReleaseCapacity {
    fn clone(&self) -> Self {
        let inner = self.inner.clone();
        ReleaseCapacity { inner }
    }
}
