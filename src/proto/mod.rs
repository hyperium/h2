mod connection;
mod framed_read;
mod framed_write;
mod ping_pong;
mod ready;
mod settings;
mod state;

pub use self::connection::{Connection, new as new_connection};
pub use self::framed_read::FramedRead;
pub use self::framed_write::FramedWrite;
pub use self::ping_pong::PingPong;
pub use self::ready::ReadySink;
pub use self::settings::Settings;
pub use self::state::State;

use frame;

/// A request or response issued by the current process.
pub struct SendMessage {
    frame: frame::Headers,
}

/// A request or response received by the current process.
pub struct PollMessage {
    frame: frame::Headers,
}

impl SendMessage {
    pub fn new(frame: frame::Headers) -> Self {
        SendMessage {
            frame: frame,
        }
    }
}
