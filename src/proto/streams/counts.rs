use super::*;
use client;

use std::marker::PhantomData;
use std::usize;

#[derive(Debug)]
pub(super) struct Counts<P>
where
    P: Peer,
{
    /// Maximum number of locally initiated streams
    max_send_streams: Option<usize>,

    /// Current number of remote initiated streams
    num_send_streams: usize,

    /// Maximum number of remote initiated streams
    max_recv_streams: Option<usize>,

    /// Current number of locally initiated streams
    num_recv_streams: usize,

    /// Task awaiting notification to open a new stream.
    blocked_open: Option<task::Task>,

    _p: PhantomData<P>,
}

impl<P> Counts<P>
where
    P: Peer,
{
    /// Create a new `Counts` using the provided configuration values.
    pub fn new(config: &Config) -> Self {
        Counts {
            max_send_streams: config.local_max_initiated,
            num_send_streams: 0,
            max_recv_streams: config.remote_max_initiated,
            num_recv_streams: 0,
            blocked_open: None,
            _p: PhantomData,
        }
    }

    /// Returns true if the receive stream concurrency can be incremented
    pub fn can_inc_num_recv_streams(&self) -> bool {
        if let Some(max) = self.max_recv_streams {
            max > self.num_recv_streams
        } else {
            true
        }
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
        if let Some(max) = self.max_send_streams {
            max > self.num_send_streams
        } else {
            true
        }
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
            self.max_send_streams = Some(val as usize);
        }
    }

    /// Run a block of code that could potentially transition a stream's state.
    ///
    /// If the stream state transitions to closed, this function will perform
    /// all necessary cleanup.
    pub fn transition<F, B, U>(&mut self, mut stream: store::Ptr<B, P>, f: F) -> U
    where
        F: FnOnce(&mut Self, &mut store::Ptr<B, P>) -> U,
    {
        let is_counted = stream.state.is_counted();

        // Run the action
        let ret = f(self, &mut stream);

        self.transition_after(stream, is_counted);

        ret
    }

    // TODO: move this to macro?
    pub fn transition_after<B>(&mut self, mut stream: store::Ptr<B, P>, is_counted: bool) {
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
        use std::usize;

        if P::is_local_init(id) {
            self.num_send_streams -= 1;

            if self.num_send_streams < self.max_send_streams.unwrap_or(usize::MAX) {
                if let Some(task) = self.blocked_open.take() {
                    task.notify();
                }
            }
        } else {
            self.num_recv_streams -= 1;
        }
    }
}

impl Counts<client::Peer> {
    pub fn poll_open_ready(&mut self) -> Async<()> {
        if !self.can_inc_num_send_streams() {
            self.blocked_open = Some(task::current());
            return Async::NotReady;
        }

        return Async::Ready(());
    }
}
