use StreamId;

#[derive(Debug)]
pub enum WindowUpdate {
    Connection { increment: usize },
    Stream { id: StreamId, increment: usize },
}

impl WindowUpdate {
    pub fn increment(&self) -> usize {
        match *self {
            WindowUpdate::Connection { increment } |
            WindowUpdate::Stream { increment, .. } => increment
        }
    }
}
