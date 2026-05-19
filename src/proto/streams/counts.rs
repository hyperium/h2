use super::*;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

#[derive(Debug)]
pub(super) struct Shared {
    /// Maximum number of locally initiated streams
    max_send_streams: AtomicUsize,

    /// Current number of remote initiated streams
    num_send_streams: AtomicUsize,

    /// Maximum number of remote initiated streams
    max_recv_streams: AtomicUsize,

    /// Current number of locally initiated streams
    num_recv_streams: AtomicUsize,

    /// Current number of streams that are held in memory.
    num_wired_streams: AtomicUsize,
}

#[derive(Debug)]
pub(super) struct Counts {
    /// Acting as a client or server. This allows us to track which values to
    /// inc / dec.
    peer: peer::Dyn,

    /// Maximum number of pending locally reset streams
    max_local_reset_streams: usize,

    /// Current number of pending locally reset streams
    num_local_reset_streams: usize,

    /// Max number of "pending accept" streams that were remotely reset
    max_remote_reset_streams: usize,

    /// Current number of "pending accept" streams that were remotely reset
    num_remote_reset_streams: usize,

    /// Maximum number of locally reset streams due to protocol error across
    /// the lifetime of the connection.
    ///
    /// When this gets exceeded, we issue GOAWAYs.
    max_local_error_reset_streams: Option<usize>,

    /// Total number of locally reset streams due to protocol error across the
    /// lifetime of the connection.
    num_local_error_reset_streams: usize,
}

impl Counts {
    /// Create a new `Counts` using the provided configuration values.
    pub fn new(peer: peer::Dyn, config: &Config) -> Self {
        Counts {
            peer,
            max_local_reset_streams: config.local_reset_max,
            num_local_reset_streams: 0,
            max_remote_reset_streams: config.remote_reset_max,
            num_remote_reset_streams: 0,
            max_local_error_reset_streams: config.local_max_error_reset_streams,
            num_local_error_reset_streams: 0,
        }
    }

    /// Returns the current peer
    pub fn peer(&self) -> peer::Dyn {
        self.peer
    }

    pub fn has_streams(&self, shared: &Shared) -> bool {
        shared.has_streams()
    }

    /// Returns true if we can issue another local reset due to protocol error.
    pub fn can_inc_num_local_error_resets(&self) -> bool {
        if let Some(max) = self.max_local_error_reset_streams {
            max > self.num_local_error_reset_streams
        } else {
            true
        }
    }

    pub fn inc_num_local_error_resets(&mut self) {
        assert!(self.can_inc_num_local_error_resets());

        // Increment the number of remote initiated streams
        self.num_local_error_reset_streams += 1;
    }

    pub(crate) fn max_local_error_resets(&self) -> Option<usize> {
        self.max_local_error_reset_streams
    }

    /// Returns true if the receive stream concurrency can be incremented
    pub fn can_inc_num_recv_streams(&self, shared: &Shared) -> bool {
        shared.max_recv_streams() > shared.num_recv_streams()
    }

    /// Increments the number of concurrent receive streams.
    ///
    /// # Panics
    ///
    /// Panics on failure as this should have been validated before hand.
    pub fn inc_num_recv_streams(&mut self, shared: &Shared, stream: &mut store::Ptr) {
        assert!(self.can_inc_num_recv_streams(shared));
        assert!(!stream.inner().is_counted);

        // Increment the number of remote initiated streams
        shared.inc_num_recv_streams();
        stream.inner_mut().is_counted = true;
    }

    /// Returns true if the send stream concurrency can be incremented
    pub fn can_inc_num_send_streams(&self, shared: &Shared) -> bool {
        shared.max_send_streams() > shared.num_send_streams()
    }

    /// Increments the number of concurrent send streams.
    ///
    /// # Panics
    ///
    /// Panics on failure as this should have been validated before hand.
    pub fn inc_num_send_streams(&mut self, shared: &Shared, stream: &mut store::Ptr) {
        assert!(self.can_inc_num_send_streams(shared));
        assert!(!stream.inner().is_counted);

        // Increment the number of remote initiated streams
        shared.inc_num_send_streams();
        stream.inner_mut().is_counted = true;
    }

    /// Returns true if the number of pending reset streams can be incremented.
    pub fn can_inc_num_reset_streams(&self) -> bool {
        self.max_local_reset_streams > self.num_local_reset_streams
    }

    /// Increments the number of pending reset streams.
    ///
    /// # Panics
    ///
    /// Panics on failure as this should have been validated before hand.
    pub fn inc_num_reset_streams(&mut self) {
        assert!(self.can_inc_num_reset_streams());

        self.num_local_reset_streams += 1;
    }

    pub(crate) fn max_remote_reset_streams(&self) -> usize {
        self.max_remote_reset_streams
    }

    /// Returns true if the number of pending REMOTE reset streams can be
    /// incremented.
    pub(crate) fn can_inc_num_remote_reset_streams(&self) -> bool {
        self.max_remote_reset_streams > self.num_remote_reset_streams
    }

    /// Increments the number of pending REMOTE reset streams.
    ///
    /// # Panics
    ///
    /// Panics on failure as this should have been validated before hand.
    pub(crate) fn inc_num_remote_reset_streams(&mut self) {
        assert!(self.can_inc_num_remote_reset_streams());

        self.num_remote_reset_streams += 1;
    }

    pub(crate) fn dec_num_remote_reset_streams(&mut self) {
        assert!(self.num_remote_reset_streams > 0);

        self.num_remote_reset_streams -= 1;
    }

    pub fn apply_remote_settings(
        &mut self,
        shared: &Shared,
        settings: &frame::Settings,
        is_initial: bool,
    ) {
        match settings.max_concurrent_streams() {
            Some(val) => shared.set_max_send_streams(val as usize),
            None if is_initial => shared.set_max_send_streams(usize::MAX),
            None => {}
        }
    }

    /// Run a block of code that could potentially transition a stream's state.
    ///
    /// If the stream state transitions to closed, this function will perform
    /// all necessary cleanup.
    ///
    /// TODO: Is this function still needed?
    pub fn transition<F, U>(&mut self, shared: &Shared, mut stream: store::Ptr, f: F) -> U
    where
        F: FnOnce(&mut Self, &mut store::Ptr) -> U,
    {
        // TODO: Does this need to be computed before performing the action?
        let is_pending_reset = stream.is_pending_reset_expiration();

        // Run the action
        let ret = f(self, &mut stream);

        self.transition_after(shared, stream, is_pending_reset);

        ret
    }

    // TODO: move this to macro?
    pub fn transition_after(
        &mut self,
        shared: &Shared,
        mut stream: store::Ptr,
        is_reset_counted: bool,
    ) {
        let is_closed = stream.is_closed();
        {
            let state = stream.shared();
            let send = stream.send();
            tracing::trace!(
                "transition_after; stream={:?}; state={:?}; is_closed={:?}; \
                 pending_send_empty={:?}; buffered_send_data={}; \
                num_recv={}; num_send={}",
                stream.id,
                state.state,
                is_closed,
                send.pending_send.is_empty(),
                send.buffered_send_data,
                shared.num_recv_streams(),
                shared.num_send_streams()
            );
        }

        if is_closed && !stream.shared().is_pending_push {
            if !stream.is_pending_reset_expiration() {
                stream.unlink();
                if is_reset_counted {
                    self.dec_num_reset_streams();
                }
            }

            let should_dec_num_streams = {
                let is_scheduled_reset = stream.shared().state.is_scheduled_reset();
                let is_counted = stream.inner().is_counted;
                let pending_operation_count = stream.shared().pending_operation_count;
                let send = stream.send();
                !is_scheduled_reset
                    && is_counted
                    && pending_operation_count == 0
                    && send.pending_send.is_empty()
                    && send.pending_op_data.is_empty()
                    && send.buffered_send_data == 0
            };

            if should_dec_num_streams {
                tracing::trace!("dec_num_streams; stream={:?}", stream.id);
                // Decrement the number of active streams.
                self.dec_num_streams(shared, &mut stream);
            }
        }

        // Release the stream if it requires releasing
        if stream.is_released() {
            stream.remove();
            shared.dec_num_wired_streams();
        }
    }

    /// Returns the maximum number of streams that can be initiated by this
    /// peer.
    pub(crate) fn max_send_streams(&self, shared: &Shared) -> usize {
        shared.max_send_streams()
    }

    /// Returns the maximum number of streams that can be initiated by the
    /// remote peer.
    pub(crate) fn max_recv_streams(&self, shared: &Shared) -> usize {
        shared.max_recv_streams()
    }

    fn dec_num_streams(&mut self, shared: &Shared, stream: &mut store::Ptr) {
        assert!(stream.inner().is_counted);

        if self.peer.is_local_init(stream.id) {
            assert!(shared.num_send_streams() > 0);
            shared.dec_num_send_streams();
            stream.inner_mut().is_counted = false;
        } else {
            assert!(shared.num_recv_streams() > 0);
            shared.dec_num_recv_streams();
            stream.inner_mut().is_counted = false;
        }
    }

    fn dec_num_reset_streams(&mut self) {
        assert!(self.num_local_reset_streams > 0);
        self.num_local_reset_streams -= 1;
    }
}

impl Shared {
    pub fn new(config: &Config) -> Self {
        Shared {
            max_send_streams: AtomicUsize::new(config.initial_max_send_streams),
            num_send_streams: AtomicUsize::new(0),
            max_recv_streams: AtomicUsize::new(config.remote_max_initiated.unwrap_or(usize::MAX)),
            num_recv_streams: AtomicUsize::new(0),
            num_wired_streams: AtomicUsize::new(0),
        }
    }

    pub fn has_streams(&self) -> bool {
        self.num_send_streams() != 0 || self.num_recv_streams() != 0
    }

    pub fn max_send_streams(&self) -> usize {
        self.max_send_streams.load(AtomicOrdering::Relaxed)
    }

    pub fn set_max_send_streams(&self, val: usize) {
        self.max_send_streams.store(val, AtomicOrdering::Relaxed);
    }

    pub fn num_send_streams(&self) -> usize {
        self.num_send_streams.load(AtomicOrdering::Relaxed)
    }

    pub fn inc_num_send_streams(&self) {
        self.num_send_streams.fetch_add(1, AtomicOrdering::Relaxed);
    }

    pub fn dec_num_send_streams(&self) {
        self.num_send_streams.fetch_sub(1, AtomicOrdering::Relaxed);
    }

    pub fn max_recv_streams(&self) -> usize {
        self.max_recv_streams.load(AtomicOrdering::Relaxed)
    }

    pub fn num_recv_streams(&self) -> usize {
        self.num_recv_streams.load(AtomicOrdering::Relaxed)
    }

    pub fn inc_num_recv_streams(&self) {
        self.num_recv_streams.fetch_add(1, AtomicOrdering::Relaxed);
    }

    pub fn dec_num_recv_streams(&self) {
        self.num_recv_streams.fetch_sub(1, AtomicOrdering::Relaxed);
    }

    #[cfg(feature = "unstable")]
    pub fn num_active_streams(&self) -> usize {
        self.num_send_streams() + self.num_recv_streams()
    }

    pub fn current_max_recv_streams(&self) -> usize {
        self.max_recv_streams()
    }

    #[cfg(feature = "unstable")]
    pub fn num_wired_streams(&self) -> usize {
        self.num_wired_streams.load(AtomicOrdering::Relaxed)
    }

    pub fn inc_num_wired_streams(&self) {
        self.num_wired_streams.fetch_add(1, AtomicOrdering::Relaxed);
    }

    pub fn dec_num_wired_streams(&self) {
        self.num_wired_streams.fetch_sub(1, AtomicOrdering::Relaxed);
    }
}
