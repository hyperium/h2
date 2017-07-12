#[derive(Clone, Copy, Debug)]
pub struct WindowUnderflow;

pub const DEFAULT_INITIAL_WINDOW_SIZE: u32 = 65_535;

#[derive(Copy, Clone, Debug)]
pub struct FlowController {
    /// Amount that may be claimed.
    window_size: u32,
    /// Amount to be removed by future increments.
    underflow: u32,
    /// The amount that has been incremented but not yet advertised (to the application or
    /// the remote).
    next_window_update: u32,
}

impl Default for FlowController {
    fn default() -> Self {
        Self::new(DEFAULT_INITIAL_WINDOW_SIZE)
    }
}

impl FlowController {
    pub fn new(window_size: u32) -> FlowController {
        FlowController {
            window_size,
            underflow: 0,
            next_window_update: 0,
        }
    }

    pub fn window_size(&self) -> u32 {
        self.window_size
    }

    /// Reduce future capacity of the window.
    ///
    /// This accomodates updates to SETTINGS_INITIAL_WINDOW_SIZE.
    pub fn shrink_window(&mut self, decr: u32) {
        self.underflow += decr;
    }

    /// Claim the provided amount from the window, if there is enough space.
    pub fn claim_window(&mut self, sz: u32) -> Result<(), WindowUnderflow> {
        if self.window_size < sz {
            return Err(WindowUnderflow);
        }

        self.window_size -= sz;
        Ok(())
    }

    /// Applies a window increment immediately.
    pub fn add_to_window(&mut self, sz: u32) {
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
    pub fn take_window_update(&mut self) -> Option<u32> {
        if self.next_window_update == 0 {
            return None;
        }

        let incr = self.next_window_update;
        self.next_window_update = 0;
        Some(incr)
    }
}
