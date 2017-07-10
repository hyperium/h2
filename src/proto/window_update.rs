use StreamId;
use frame;

#[derive(Debug)]
pub enum WindowUpdate {
    Connection { increment: u32 },
    Stream { id: StreamId, increment: u32 },
}

impl WindowUpdate {
    pub fn increment(&self) -> u32 {
        match *self {
            WindowUpdate::Connection { increment } |
            WindowUpdate::Stream { increment, .. } => increment
        }
    }
}

impl From<WindowUpdate> for frame::WindowUpdate {
    fn from(src: WindowUpdate) -> Self {
        match src {
            WindowUpdate::Connection { increment } => {
                frame::WindowUpdate::new(StreamId::zero(), increment)
            }
            WindowUpdate::Stream { id, increment } => {
                frame::WindowUpdate::new(id, increment)
            }
        }
    }
}

impl From<WindowUpdate> for frame::Frame {
    fn from(src: WindowUpdate) -> Self {
        frame::Frame::WindowUpdate(src.into())
    }
}
