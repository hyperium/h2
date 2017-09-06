use frame::Reason;
use proto::{MAX_WINDOW_SIZE, WindowSize};

// We don't want to send WINDOW_UPDATE frames for tiny changes, but instead
// aggregate them when the changes are significant. Many implementations do
// this by keeping a "ratio" of the update version the allowed window size.
//
// While some may wish to represent this ratio as percentage, using a f32,
// we skip having to deal with float math and stick to integers. To do so,
// the "ratio" is represented by 2 i32s, split into the numerator and
// denominator. For example, a 50% ratio is simply represented as 1/2.
//
// An example applying this ratio: If a stream has an allowed window size of
// 100 bytes, WINDOW_UPDATE frames are scheduled when the unclaimed change
// becomes greater than 1/2, or 50 bytes.
const UNCLAIMED_NUMERATOR: i32 = 1;
const UNCLAIMED_DENOMINATOR: i32 = 2;

#[test]
fn sanity_unclaimed_ratio() {
    assert!(UNCLAIMED_NUMERATOR < UNCLAIMED_DENOMINATOR);
    assert!(UNCLAIMED_NUMERATOR >= 0);
    assert!(UNCLAIMED_DENOMINATOR > 0);
}

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

    /// If a WINDOW_UPDATE frame should be sent, returns a positive number
    /// representing the increment to be used.
    ///
    /// If there is no available bytes to be reclaimed, or the number of
    /// available bytes does not reach the threshold, this returns `None`.
    ///
    /// This represents pending outbound WINDOW_UPDATE frames.
    pub fn unclaimed_capacity(&self) -> Option<WindowSize> {
        let available = self.available as i32;

        if self.window_size >= available {
            return None;
        }

        let unclaimed = available - self.window_size;
        let threshold = self.window_size / UNCLAIMED_DENOMINATOR
            * UNCLAIMED_NUMERATOR;

        if unclaimed < threshold {
            None
        } else {
            Some(unclaimed as WindowSize)
        }
    }

    /// Increase the window size.
    ///
    /// This is called after receiving a WINDOW_UPDATE frame
    pub fn inc_window(&mut self, sz: WindowSize) -> Result<(), Reason> {
        let (val, overflow) = self.window_size.overflowing_add(sz as i32);

        if overflow {
            return Err(Reason::FlowControlError);
        }

        if val > MAX_WINDOW_SIZE as i32 {
            return Err(Reason::FlowControlError);
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
