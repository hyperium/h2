use ConnectionError;
use proto::*;

#[derive(Copy, Clone, Debug)]
pub struct FlowControl {
    /// Amount that may be claimed.
    window_size: WindowSize,

    /// Amount to be removed by future increments.
    underflow: WindowSize,

    /// The amount that has been incremented but not yet advertised (to the
    /// application or the remote).
    next_window_update: WindowSize,
}

impl FlowControl {
    pub fn new(window_size: WindowSize) -> FlowControl {
        FlowControl {
            window_size,
            underflow: 0,
            next_window_update: 0,
        }
    }

    pub fn has_capacity(&self) -> bool {
        self.window_size > 0
    }

    pub fn window_size(&self) -> WindowSize {
        self.window_size
    }

    /// Returns true iff `claim_window(sz)` would return succeed.
    pub fn ensure_window<T>(&mut self, sz: WindowSize, err: T) -> Result<(), ConnectionError>
        where T: Into<ConnectionError>,
    {
        if sz <= self.window_size {
            Ok(())
        } else {
            Err(err.into())
        }
    }

    /// Claims the provided amount from the window, if there is enough space.
    ///
    /// Fails when `apply_window_update()` hasn't returned at least `sz` more bytes than
    /// have been previously claimed.
    pub fn claim_window<T>(&mut self, sz: WindowSize, err: T)
        -> Result<(), ConnectionError>
        where T: Into<ConnectionError>,
    {
        self.ensure_window(sz, err)?;

        self.window_size -= sz;
        Ok(())
    }

    /// Increase the _unadvertised_ window capacity.
    pub fn expand_window(&mut self, sz: WindowSize)
        -> Result<(), ConnectionError>
    {
        // TODO: Handle invalid increment
        if sz <= self.underflow {
            self.underflow -= sz;
            return;
        }

        let added = sz - self.underflow;
        self.next_window_update += added;
        self.underflow = 0;
    }

    /*
    /// Obtains the unadvertised window update.
    ///
    /// This does not apply the window update to `self`.
    pub fn peek_window_update(&mut self) -> Option<WindowSize> {
        if self.next_window_update == 0 {
            None
        } else {
            Some(self.next_window_update)
        }
    }
    */

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
