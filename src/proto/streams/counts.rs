use super::*;

use std::usize;

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
}

impl Counts {
    /// Create a new `Counts` using the provided configuration values.
    pub fn new(peer: peer::Dyn, config: &Config) -> Self {
        Counts {
            peer,
            max_send_streams: config.local_max_initiated.unwrap_or(usize::MAX),
            num_send_streams: 0,
            max_recv_streams: config.remote_max_initiated.unwrap_or(usize::MAX),
            num_recv_streams: 0,
        }
    }

    /// Returns the current peer
    pub fn peer(&self) -> peer::Dyn {
        self.peer
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
    pub fn inc_num_recv_streams(&mut self) {
        assert!(self.can_inc_num_recv_streams());

        // Increment the number of remote initiated streams
        self.num_recv_streams += 1;
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
    pub fn inc_num_send_streams(&mut self) {
        assert!(self.can_inc_num_send_streams());

        // Increment the number of remote initiated streams
        self.num_send_streams += 1;
    }

    pub fn apply_remote_settings(&mut self, settings: &frame::Settings) {
        if let Some(val) = settings.max_concurrent_streams() {
            self.max_send_streams = val as usize;
        }
    }

    /// Run a block of code that could potentially transition a stream's state.
    ///
    /// If the stream state transitions to closed, this function will perform
    /// all necessary cleanup.
    pub fn transition<F, U>(&mut self, mut stream: store::Ptr, f: F) -> U
    where
        F: FnOnce(&mut Self, &mut store::Ptr) -> U,
    {
        let is_counted = stream.is_counted();

        // Run the action
        let ret = f(self, &mut stream);

        self.transition_after(stream, is_counted);

        ret
    }

    // TODO: move this to macro?
    pub fn transition_after(&mut self, mut stream: store::Ptr, is_counted: bool) {
        if stream.is_closed() {
            stream.unlink();

            if is_counted {
                // Decrement the number of active streams.
                self.dec_num_streams(stream.id);
            }
        }

        // Release the stream if it requires releasing
        if stream.is_released() {
            stream.remove();
        }
    }

    fn dec_num_streams(&mut self, id: StreamId) {
        if self.peer.is_local_init(id) {
            self.num_send_streams -= 1;
        } else {
            self.num_recv_streams -= 1;
        }
    }
}
