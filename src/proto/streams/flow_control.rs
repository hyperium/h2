use ConnectionError;
use proto::*;

use std::cmp;

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
        assert!(self.window_size() >= self.available + capacity);
        self.available += capacity;
    }

    /// Update the window size.
    ///
    /// This is called after receiving a WINDOW_UPDATE frame
    pub fn update_window(&mut self, sz: WindowSize) -> Result<(), ConnectionError> {
        // TODO: Handle invalid increment
        self.window_size += sz as i32;
        Ok(())
    }

    /// Decrements the **available** window.
    ///
    /// This does not decrement the actual window as visible to the peer. This
    /// function should be called before sending data into the prioritization
    /// layer.
    pub fn send<E>(&mut self, sz: WindowSize, err: E) -> Result<(), E>
    {
        if self.available < sz {
            return Err(err);
        }

        self.available -= sz;
        Ok(())
    }

    /*
    pub fn with_window_size(window_size: WindowSize) -> FlowControl {
        FlowControl {
            window_size: window_size as i32,
            available:
        }
    }
    */

    /*

    /// Set the window size.
    ///
    /// This should only be called when setting the initial window size
    pub fn set_window_size(&mut self, val: WindowSize) {
        self.window_size = val as i32;
        self.advertised = val;
    }

    fn window_size(&self) -> WindowSize {
        if self.window_size < 0 {
            0
        } else {
            self.window_size as WindowSize
        }
    }

    pub fn advertise(&mut self, val: WindowSize) {
        self.advertised = cmp::min(val, self.window_size());
    }

    /// Return the advertised window size
    pub fn advertised(&self) -> WindowSize {
        self.advertised
    }

    pub fn is_fully_advertised(&self) -> bool {
        self.advertised() >= self.window_size()
    }

    /// Increase the window capacity
    pub fn expand_window(&mut self, sz: WindowSize)
        -> Result<(), ConnectionError>
    {
        // TODO: Handle invalid increment
        self.window_size += sz as i32;
        Ok(())
    }

    pub fn observe_window(&mut self) -> WindowSize {
        unimplemented!();
    }
    */

    /*

    pub fn has_capacity(&self) -> bool {
        self.effective_window_size() > 0
    }

    pub fn effective_window_size(&self) -> WindowSize {
        let plus = self.window_size + self.next_window_update;

        if self.underflow >= plus {
            return 0;
        }

        plus - self.underflow
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

    /// Reduce future capacity of the window.
    ///
    /// This accomodates updates to SETTINGS_INITIAL_WINDOW_SIZE.
    pub fn shrink_window(&mut self, dec: WindowSize) {
        /*
        if decr < self.next_window_update {
            self.next_window_update -= decr
        } else {
            self.underflow += decr - self.next_window_update;
            self.next_window_update = 0;
        }
        */
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
            return Ok(());
        }

        let added = sz - self.underflow;
        self.next_window_update += added;
        self.underflow = 0;

        Ok(())
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
    */
}
