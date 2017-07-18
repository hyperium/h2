use proto::WindowSize;

#[derive(Clone, Copy, Debug)]
pub struct WindowUnderflow;

pub const DEFAULT_INITIAL_WINDOW_SIZE: WindowSize = 65_535;

#[derive(Copy, Clone, Debug)]
pub struct FlowControlState {
    /// Amount that may be claimed.
    window_size: WindowSize,
    /// Amount to be removed by future increments.
    underflow: WindowSize,
    /// The amount that has been incremented but not yet advertised (to the application or
    /// the remote).
    next_window_update: WindowSize,
}

impl Default for FlowControlState {
    fn default() -> Self {
        Self::with_initial_size(DEFAULT_INITIAL_WINDOW_SIZE)
    }
}

impl FlowControlState {
    pub fn with_initial_size(window_size: WindowSize) -> FlowControlState {
        FlowControlState {
            window_size,
            underflow: 0,
            next_window_update: 0,
        }
    }

    pub fn with_next_update(next_window_update: WindowSize) -> FlowControlState {
        FlowControlState {
            window_size: 0,
            underflow: 0,
            next_window_update,
        }
    }

    /// Reduce future capacity of the window.
    ///
    /// This accomodates updates to SETTINGS_INITIAL_WINDOW_SIZE.
    pub fn shrink_window(&mut self, decr: WindowSize) {
        if decr < self.next_window_update {
            self.next_window_update -= decr
        } else {
            self.underflow += decr - self.next_window_update;
            self.next_window_update = 0;
        }
    }

    /// Returns true iff `claim_window(sz)` would return succeed.
    pub fn check_window(&mut self, sz: WindowSize) -> bool {
        sz <= self.window_size
    }

    /// Claims the provided amount from the window, if there is enough space.
    ///
    /// Fails when `apply_window_update()` hasn't returned at least `sz` more bytes than
    /// have been previously claimed.
    pub fn claim_window(&mut self, sz: WindowSize) -> Result<(), WindowUnderflow> {
        if !self.check_window(sz) {
            return Err(WindowUnderflow);
        }

        self.window_size -= sz;
        Ok(())
    }

    /// Increase the _unadvertised_ window capacity.
    pub fn expand_window(&mut self, sz: WindowSize) {
        if sz <= self.underflow {
            self.underflow -= sz;
            return;
        }

        let added = sz - self.underflow;
        self.next_window_update += added;
        self.underflow = 0;
    }

    /// Obtains and applies an unadvertised window update.
    pub fn apply_window_update(&mut self) -> Option<WindowSize> {
        if self.next_window_update == 0 {
            return None;
        }

        let incr = self.next_window_update;
        self.next_window_update = 0;
        self.window_size += incr;
        Some(incr)
    }
}

#[test]
fn test_with_initial_size() {
    let mut fc = FlowControlState::with_initial_size(10);

    fc.expand_window(8);
    assert_eq!(fc.window_size, 10);
    assert_eq!(fc.next_window_update, 8);

    assert_eq!(fc.apply_window_update(), Some(8));
    assert_eq!(fc.window_size, 18);
    assert_eq!(fc.next_window_update, 0);

    assert!(fc.claim_window(13).is_ok());
    assert_eq!(fc.window_size, 5);
    assert_eq!(fc.next_window_update, 0);
    assert!(fc.apply_window_update().is_none());
}

#[test]
fn test_with_next_update() {
    let mut fc = FlowControlState::with_next_update(10);

    fc.expand_window(8);
    assert_eq!(fc.window_size, 0);
    assert_eq!(fc.next_window_update, 18);

    assert_eq!(fc.apply_window_update(), Some(18));
    assert_eq!(fc.window_size, 18);
    assert_eq!(fc.next_window_update, 0);
}

#[test]
fn test_grow_accumulates() {
    let mut fc = FlowControlState::with_initial_size(5);

    // Updates accumulate, though the window is not made immediately available.  Trying to
    // claim data not returned by apply_window_update results in an underflow.

    fc.expand_window(2);
    assert_eq!(fc.window_size, 5);
    assert_eq!(fc.next_window_update, 2);

    fc.expand_window(6);
    assert_eq!(fc.window_size, 5);
    assert_eq!(fc.next_window_update, 8);

    assert!(fc.claim_window(13).is_err());
    assert_eq!(fc.window_size, 5);
    assert_eq!(fc.next_window_update, 8);

    assert_eq!(fc.apply_window_update(), Some(8));
    assert_eq!(fc.window_size, 13);
    assert_eq!(fc.next_window_update, 0);

    assert!(fc.claim_window(13).is_ok());
    assert_eq!(fc.window_size, 0);
    assert_eq!(fc.next_window_update, 0);
}

#[test]
fn test_shrink() {
    let mut fc = FlowControlState::with_initial_size(5);
    assert_eq!(fc.window_size, 5);
    assert_eq!(fc.next_window_update, 0);

    fc.expand_window(3);
    assert_eq!(fc.window_size, 5);
    assert_eq!(fc.next_window_update, 3);
    assert_eq!(fc.underflow, 0);

    fc.shrink_window(8);
    assert_eq!(fc.window_size, 5);
    assert_eq!(fc.next_window_update, 0);
    assert_eq!(fc.underflow, 5);

    assert!(fc.claim_window(5).is_ok());
    assert_eq!(fc.window_size, 0);
    assert_eq!(fc.next_window_update, 0);
    assert_eq!(fc.underflow, 5);

    fc.expand_window(8);
    assert_eq!(fc.window_size, 0);
    assert_eq!(fc.next_window_update, 3);
    assert_eq!(fc.underflow, 0);
}
