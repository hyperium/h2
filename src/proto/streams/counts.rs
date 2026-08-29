use super::*;

#[derive(Debug)]
struct Budget {
    available: usize,
    max: usize,
}

#[derive(Debug)]
pub(super) struct BudgetExhausted;

impl Budget {
    fn new(max: usize) -> Self {
        Budget {
            available: max,
            max,
        }
    }

    fn consume(&mut self, amount: usize) -> Result<(), BudgetExhausted> {
        self.available = self.available.checked_sub(amount).ok_or(BudgetExhausted)?;
        Ok(())
    }

    fn replenish(&mut self, amount: usize) {
        self.available = self.available.saturating_add(amount).min(self.max);
    }
}

#[derive(Debug)]
pub(super) struct Counts {
    /// Acting as a client or server. This allows us to track which values to
    /// inc / dec.
    peer: peer::Dyn,

    /// Maximum number of locally initiated streams
    max_send_streams: usize,

    /// Current number of remote initiated streams
    num_send_streams: usize,

    /// Maximum number of remote initiated streams
    max_recv_streams: usize,

    /// Current number of locally initiated streams
    num_recv_streams: usize,

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

    /// connection-level budget for DATA framing overhead.
    data_frame_budget: Budget,

    /// payload length below which a received DATA frame is charged framing
    /// overhead against `data_frame_budget`.
    data_frame_overhead_threshold: usize,

    /// Number of empty, non-final DATA frames received over the lifetime of
    /// the connection.
    num_recv_empty_data_frames: usize,
}

impl Counts {
    /// Create a new `Counts` using the provided configuration values.
    pub fn new(peer: peer::Dyn, config: &Config) -> Self {
        Counts {
            peer,
            max_send_streams: config.initial_max_send_streams,
            num_send_streams: 0,
            max_recv_streams: config.remote_max_initiated.unwrap_or(usize::MAX),
            num_recv_streams: 0,
            max_local_reset_streams: config.local_reset_max,
            num_local_reset_streams: 0,
            max_remote_reset_streams: config.remote_reset_max,
            num_remote_reset_streams: 0,
            max_local_error_reset_streams: config.local_max_error_reset_streams,
            num_local_error_reset_streams: 0,
            data_frame_budget: Budget::new(config.data_frame_budget),
            data_frame_overhead_threshold: config.data_frame_overhead_threshold,
            num_recv_empty_data_frames: 0,
        }
    }

    /// Records the framing overhead of a DATA frame.
    pub fn record_data_frame(&mut self, payload_len: usize) -> Result<(), BudgetExhausted> {
        if payload_len == 0 {
            self.num_recv_empty_data_frames = self
                .num_recv_empty_data_frames
                .checked_add(1)
                .ok_or(BudgetExhausted)?;
            if self.num_recv_empty_data_frames > MAX_RECV_EMPTY_DATA_FRAMES {
                return Err(BudgetExhausted);
            }
            Ok(())
        } else if payload_len < self.data_frame_overhead_threshold {
            self.data_frame_budget
                .consume(self.data_frame_overhead_threshold - payload_len)
        } else {
            self.data_frame_budget
                .replenish(payload_len - self.data_frame_overhead_threshold);
            Ok(())
        }
    }

    /// Releases the framing overhead of a DATA frame that is no longer
    /// buffered internally.
    pub fn release_data_frame(&mut self, payload_len: usize) {
        if payload_len != 0 && payload_len < self.data_frame_overhead_threshold {
            self.data_frame_budget
                .replenish(self.data_frame_overhead_threshold - payload_len);
        }
    }

    /// Returns true when the next opened stream will reach capacity of outbound streams
    ///
    /// The number of client send streams is incremented in prioritize; send_request has to guess if
    /// it should wait before allowing another request to be sent.
    pub fn next_send_stream_will_reach_capacity(&self) -> bool {
        self.max_send_streams <= (self.num_send_streams + 1)
    }

    /// Returns the current peer
    pub fn peer(&self) -> peer::Dyn {
        self.peer
    }

    pub fn has_streams(&self) -> bool {
        self.num_send_streams != 0 || self.num_recv_streams != 0
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
    pub fn can_inc_num_recv_streams(&self) -> bool {
        self.max_recv_streams > self.num_recv_streams
    }

    /// Increments the number of concurrent receive streams.
    ///
    /// # Panics
    ///
    /// Panics on failure as this should have been validated before hand.
    pub fn inc_num_recv_streams(&mut self, stream: &mut store::Ptr) {
        assert!(self.can_inc_num_recv_streams());
        assert!(!stream.is_counted);

        // Increment the number of remote initiated streams
        self.num_recv_streams += 1;
        stream.is_counted = true;
    }

    /// Returns true if the send stream concurrency can be incremented
    pub fn can_inc_num_send_streams(&self) -> bool {
        self.max_send_streams > self.num_send_streams
    }

    /// Increments the number of concurrent send streams.
    ///
    /// # Panics
    ///
    /// Panics on failure as this should have been validated before hand.
    pub fn inc_num_send_streams(&mut self, stream: &mut store::Ptr) {
        assert!(self.can_inc_num_send_streams());
        assert!(!stream.is_counted);

        // Increment the number of remote initiated streams
        self.num_send_streams += 1;
        stream.is_counted = true;
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

    pub fn apply_remote_settings(&mut self, settings: &frame::Settings, is_initial: bool) {
        match settings.max_concurrent_streams() {
            Some(val) => self.max_send_streams = val as usize,
            None if is_initial => self.max_send_streams = usize::MAX,
            None => {}
        }
    }

    /// Run a block of code that could potentially transition a stream's state.
    ///
    /// If the stream state transitions to closed, this function will perform
    /// all necessary cleanup.
    ///
    /// TODO: Is this function still needed?
    pub fn transition<F, U>(&mut self, mut stream: store::Ptr, f: F) -> U
    where
        F: FnOnce(&mut Self, &mut store::Ptr) -> U,
    {
        // TODO: Does this need to be computed before performing the action?
        let is_pending_reset = stream.is_pending_reset_expiration();

        // Run the action
        let ret = f(self, &mut stream);

        self.transition_after(stream, is_pending_reset);

        ret
    }

    // TODO: move this to macro?
    pub fn transition_after(&mut self, mut stream: store::Ptr, is_reset_counted: bool) {
        tracing::trace!(
            "transition_after; stream={:?}; state={:?}; is_closed={:?}; \
             pending_send_empty={:?}; buffered_send_data={}; \
             num_recv={}; num_send={}",
            stream.id,
            stream.state,
            stream.is_closed(),
            stream.pending_send.is_empty(),
            stream.buffered_send_data,
            self.num_recv_streams,
            self.num_send_streams
        );

        if stream.is_closed() {
            if !stream.is_pending_reset_expiration() {
                stream.unlink();
                if is_reset_counted {
                    self.dec_num_reset_streams();
                }
            }

            if !stream.state.is_scheduled_reset() && stream.is_counted {
                tracing::trace!("dec_num_streams; stream={:?}", stream.id);
                // Decrement the number of active streams.
                self.dec_num_streams(&mut stream);
            }
        }

        // Release the stream if it requires releasing
        if stream.is_released() {
            stream.remove();
        }
    }

    /// Returns the maximum number of streams that can be initiated by this
    /// peer.
    pub(crate) fn max_send_streams(&self) -> usize {
        self.max_send_streams
    }

    /// Returns the maximum number of streams that can be initiated by the
    /// remote peer.
    pub(crate) fn max_recv_streams(&self) -> usize {
        self.max_recv_streams
    }

    fn dec_num_streams(&mut self, stream: &mut store::Ptr) {
        assert!(stream.is_counted);

        if self.peer.is_local_init(stream.id) {
            assert!(self.num_send_streams > 0);
            self.num_send_streams -= 1;
            stream.is_counted = false;
        } else {
            assert!(self.num_recv_streams > 0);
            self.num_recv_streams -= 1;
            stream.is_counted = false;
        }
    }

    fn dec_num_reset_streams(&mut self) {
        assert!(self.num_local_reset_streams > 0);
        self.num_local_reset_streams -= 1;
    }
}

impl Drop for Counts {
    fn drop(&mut self) {
        use std::thread;

        if !thread::panicking() {
            debug_assert!(!self.has_streams());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::DEFAULT_INITIAL_WINDOW_SIZE;

    fn counts() -> Counts {
        counts_with_threshold(DEFAULT_DATA_FRAME_OVERHEAD_THRESHOLD)
    }

    fn counts_with_threshold(threshold: usize) -> Counts {
        Counts::new(
            peer::Dyn::Server,
            &Config {
                initial_max_send_streams: 0,
                local_max_buffer_size: 0,
                local_next_stream_id: 2.into(),
                local_push_enabled: false,
                extended_connect_protocol_enabled: false,
                local_reset_duration: Duration::ZERO,
                local_reset_max: 0,
                remote_reset_max: 0,
                remote_init_window_sz: DEFAULT_INITIAL_WINDOW_SIZE,
                remote_max_initiated: None,
                local_max_error_reset_streams: None,
                data_frame_budget: threshold * 100,
                data_frame_overhead_threshold: threshold,
            },
        )
    }

    #[test]
    fn budget_is_bounded() {
        let mut budget = Budget::new(10);

        budget.consume(4).unwrap();
        budget.replenish(20);
        assert_eq!(budget.available, 10);
    }

    #[test]
    fn budget_reports_exhaustion_without_underflowing() {
        let mut budget = Budget::new(10);

        budget.consume(10).unwrap();
        assert!(budget.consume(1).is_err());
        assert_eq!(budget.available, 0);
    }

    #[test]
    fn good_sized_data_frames_do_not_exhaust_budget() {
        let mut counts = counts();

        for _ in 0..1_000_000 {
            counts
                .record_data_frame(DEFAULT_DATA_FRAME_OVERHEAD_THRESHOLD)
                .unwrap();
        }
    }

    #[test]
    fn consumed_small_data_frames_do_not_exhaust_budget() {
        let mut counts = counts();

        for _ in 0..1_000_000 {
            counts.record_data_frame(1).unwrap();
            counts.release_data_frame(1);
        }
    }

    #[test]
    fn empty_data_frames_do_not_consume_data_frame_budget() {
        let mut counts = counts();
        counts.data_frame_budget = Budget::new(0);

        for _ in 0..MAX_RECV_EMPTY_DATA_FRAMES {
            counts.record_data_frame(0).unwrap();
        }

        // Empty frames have their own limit, while a non-empty small frame
        // still consumes the independently configured DATA frame budget.
        assert!(counts.record_data_frame(0).is_err());
        assert!(counts.record_data_frame(1).is_err());
    }

    #[test]
    fn large_data_frames_do_not_replenish_empty_data_frame_limit() {
        let mut counts = counts();

        for _ in 0..MAX_RECV_EMPTY_DATA_FRAMES {
            counts.record_data_frame(0).unwrap();
            counts
                .record_data_frame(DEFAULT_DATA_FRAME_OVERHEAD_THRESHOLD * 2)
                .unwrap();
        }
        assert!(counts.record_data_frame(0).is_err());
    }

    #[test]
    fn a_lowered_threshold_stops_charging_frames_at_or_above_it() {
        let threshold = 16;
        let mut counts = counts_with_threshold(threshold);

        // At or above the threshold nothing is charged, so an unlimited number
        // of such frames may stay buffered unread. This is the point of making
        // the threshold configurable: a peer that knows the smallest payload it
        // legitimately receives can put the threshold at or below it.
        for _ in 0..1_000_000 {
            counts.record_data_frame(threshold).unwrap();
        }
        assert_eq!(counts.data_frame_budget.available, threshold * 100);
    }

    #[test]
    fn a_lowered_threshold_still_charges_frames_below_it() {
        let threshold = 16;
        let mut counts = counts_with_threshold(threshold);

        // The other direction: shortening the charged range must not turn the
        // charge off for the payload sizes that remain inside it.
        let mut sent = 0;
        while counts.record_data_frame(1).is_ok() {
            sent += 1;
            assert!(
                sent < 10_000,
                "budget never ran out for sub-threshold frames"
            );
        }
        assert_eq!(sent, (threshold * 100) / (threshold - 1));
    }

    #[test]
    fn the_threshold_does_not_change_the_empty_data_frame_limit() {
        // Empty frames are limited by count, not by budget, so the number a
        // peer may send must be identical for every threshold. Without this,
        // lowering the threshold to admit small legitimate frames could be
        // read as weakening the empty-frame limit.
        for threshold in [1, 16, DEFAULT_DATA_FRAME_OVERHEAD_THRESHOLD, 1024] {
            let mut counts = counts_with_threshold(threshold);
            for _ in 0..MAX_RECV_EMPTY_DATA_FRAMES {
                counts.record_data_frame(0).unwrap();
            }
            assert!(
                counts.record_data_frame(0).is_err(),
                "threshold {threshold} changed the empty DATA frame limit"
            );
        }
    }

    #[test]
    fn released_frames_replenish_at_the_configured_threshold() {
        let threshold = 16;
        let mut counts = counts_with_threshold(threshold);

        // Record and release must use the same threshold, or the budget drifts
        // in one direction over the life of the connection.
        for _ in 0..1_000_000 {
            counts.record_data_frame(1).unwrap();
            counts.release_data_frame(1);
        }
        assert_eq!(counts.data_frame_budget.available, threshold * 100);
    }
}
