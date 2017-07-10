#[derive(Clone, Copy, Debug)]
pub struct WindowUnderflow;

#[derive(Copy, Clone, Debug)]
pub struct FlowController {
    window_size: u32,
    underflow: u32,
}

impl FlowController {
    pub fn new(window_size: u32) -> FlowController {
        FlowController {
            window_size,
            underflow: 0,
        }
    }

    pub fn shrink(&mut self, sz: u32) {
        self.underflow += sz;
    }

    pub fn consume(&mut self, sz: u32) -> Result<(), WindowUnderflow> {
        if self.window_size < sz {
            return Err(WindowUnderflow);
        }

        self.window_size -= sz;
        Ok(())
    }

    pub fn increment(&mut self, sz: u32) {
        if sz <= self.underflow {
            self.underflow -= sz;
            return;
        }

        self.window_size += sz - self.underflow;
        self.underflow = 0;
    }
}
