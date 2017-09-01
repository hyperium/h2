use frame::Reason;
use frame::Reason::*;
use proto::*;

#[derive(Copy, Clone, Debug)]
pub struct FlowControl {
    /// Window size as indicated by the peer. This can go negative.
    window_size: i32,

    /// The amount of the window that is currently available to consume.
    available: WindowSize,
}

impl FlowControl {
    pub fn new() -> FlowControl {
        FlowControl {
            window_size: 0,
            available: 0,
        }
    }

    /// Returns the window size as known by the peer
    pub fn window_size(&self) -> WindowSize {
        if self.window_size < 0 {
            0
        } else {
            self.window_size as WindowSize
        }
    }

    /// Returns the window size available to the consumer
    pub fn available(&self) -> WindowSize {
        self.available
    }

    /// Returns true if there is unavailable window capacity
    pub fn has_unavailable(&self) -> bool {
        if self.window_size < 0 {
            return false;
        }

        self.window_size as WindowSize > self.available
    }

    pub fn claim_capacity(&mut self, capacity: WindowSize) {
        assert!(self.available >= capacity);
        self.available -= capacity;
    }

    pub fn assign_capacity(&mut self, capacity: WindowSize) {
        self.available += capacity;
    }

    /// Returns the number of bytes available but not assigned to the window.
    ///
    /// This represents pending outbound WINDOW_UPDATE frames.
    pub fn unclaimed_capacity(&self) -> WindowSize {
        let available = self.available as i32;

        if self.window_size >= available {
            return 0;
        }

        (available - self.window_size) as WindowSize
    }

    /// Increase the window size.
    ///
    /// This is called after receiving a WINDOW_UPDATE frame
    pub fn inc_window(&mut self, sz: WindowSize) -> Result<(), Reason> {
        let (val, overflow) = self.window_size.overflowing_add(sz as i32);

        if overflow {
            return Err(FlowControlError);
        }

        if val > MAX_WINDOW_SIZE as i32 {
            return Err(FlowControlError);
        }

        trace!("inc_window; sz={}; old={}; new={}", sz, self.window_size, val);

        self.window_size = val;
        Ok(())
    }

    /// Decrement the window size.
    ///
    /// This is called after receiving a SETTINGS frame with a lower
    /// INITIAL_WINDOW_SIZE value.
    pub fn dec_window(&mut self, sz: WindowSize) {
        // This should not be able to overflow `window_size` from the bottom.
        self.window_size -= sz as i32;
    }

    /// Decrements the window reflecting data has actually been sent. The caller
    /// must ensure that the window has capacity.
    pub fn send_data(&mut self, sz: WindowSize) {
        trace!("send_data; sz={}; window={}; available={}",
               sz, self.window_size, self.available);

        // Ensure that the argument is correct
        assert!(sz <= self.window_size as WindowSize);

        // Update values
        self.window_size -= sz as i32;
        self.available -= sz;
    }
}
