use proto::WindowSize;

#[derive(Clone, Copy, Debug)]
pub struct WindowUnderflow;

pub const DEFAULT_INITIAL_WINDOW_SIZE: WindowSize = 65_535;

#[derive(Copy, Clone, Debug)]
pub struct FlowController {
    /// Amount that may be claimed.
    window_size: WindowSize,
    /// Amount to be removed by future increments.
    underflow: WindowSize,
    /// The amount that has been incremented but not yet advertised (to the application or
    /// the remote).
    next_window_update: WindowSize,
}

impl Default for FlowController {
    fn default() -> Self {
        Self::new(DEFAULT_INITIAL_WINDOW_SIZE)
    }
}

impl FlowController {
    pub fn new(window_size: WindowSize) -> FlowController {
        FlowController {
            window_size,
            underflow: 0,
            next_window_update: 0,
        }
    }

    /// Reduce future capacity of the window.
    ///
    /// This accomodates updates to SETTINGS_INITIAL_WINDOW_SIZE.
    pub fn shrink_window(&mut self, decr: WindowSize) {
        self.underflow += decr;
    }

    /// Claims the provided amount from the window, if there is enough space.
    ///
    /// Fails when `take_window_update()` hasn't returned at least `sz` more bytes than
    /// have been previously claimed.
    pub fn claim_window(&mut self, sz: WindowSize) -> Result<(), WindowUnderflow> {
        if self.window_size < sz {
            return Err(WindowUnderflow);
        }

        self.window_size -= sz;
        Ok(())
    }

    /// Applies a window increment immediately.
    pub fn increment_window_size(&mut self, sz: WindowSize) {
        if sz <= self.underflow {
            self.underflow -= sz;
            return;
        }

        let added = sz - self.underflow;
        self.window_size += added;
        self.next_window_update += added;
        self.underflow = 0;
    }

    /// Obtains and clears an unadvertised window update.
    pub fn take_window_update(&mut self) -> Option<WindowSize> {
        if self.next_window_update == 0 {
            return None;
        }

        let incr = self.next_window_update;
        self.next_window_update = 0;
        Some(incr)
    }
}

#[test]
fn test() {
    let mut fc = FlowController::new(65_535);

}
