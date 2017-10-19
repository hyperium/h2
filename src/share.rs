use frame::Reason;
use proto::{self, WindowSize};

use bytes::{Bytes, IntoBuf};
use futures::{self, Poll, Async};
use http::{HeaderMap};

use std::fmt;

#[derive(Debug)]
pub struct Stream<B: IntoBuf> {
    inner: proto::StreamRef<B::Buf>,
}

pub struct Body<B: IntoBuf> {
    inner: ReleaseCapacity<B>,
}

#[derive(Debug)]
pub struct ReleaseCapacity<B: IntoBuf> {
    inner: proto::StreamRef<B::Buf>,
}

// ===== impl Stream =====

impl<B: IntoBuf> Stream<B> {
    pub(crate) fn new(inner: proto::StreamRef<B::Buf>) -> Self {
        Stream { inner }
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

// ===== impl Body =====

impl<B: IntoBuf> Body<B> {
    pub(crate) fn new(inner: ReleaseCapacity<B>) -> Self {
        Body { inner }
    }

    pub fn is_empty(&self) -> bool {
        // If the recv side is closed and the receive queue is empty, the body is empty.
        self.inner.inner.body_is_empty()
    }

    pub fn release_capacity(&mut self) -> &mut ReleaseCapacity<B> {
        &mut self.inner
    }

    /// Poll trailers
    ///
    /// This function **must** not be called until `Body::poll` returns `None`.
    pub fn poll_trailers(&mut self) -> Poll<Option<HeaderMap>, ::Error> {
        self.inner.inner.poll_trailers().map_err(Into::into)
    }
}

impl<B: IntoBuf> futures::Stream for Body<B> {
    type Item = Bytes;
    type Error = ::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        self.inner.inner.poll_data().map_err(Into::into)
    }
}

impl<B: IntoBuf> fmt::Debug for Body<B>
where B: fmt::Debug,
      B::Buf: fmt::Debug,
{
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("Body")
            .field("inner", &self.inner)
            .finish()
    }
}

// ===== impl ReleaseCapacity =====

impl<B: IntoBuf> ReleaseCapacity<B> {
    pub(crate) fn new(inner: proto::StreamRef<B::Buf>) -> Self {
        ReleaseCapacity { inner }
    }

    pub fn release_capacity(&mut self, sz: usize) -> Result<(), ::Error> {
        self.inner
            .release_capacity(sz as proto::WindowSize)
            .map_err(Into::into)
    }
}

impl<B: IntoBuf> Clone for ReleaseCapacity<B> {
    fn clone(&self) -> Self {
        let inner = self.inner.clone();
        ReleaseCapacity { inner }
    }
}
