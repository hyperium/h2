mod connection;
mod framed_read;
mod framed_write;
mod handshake;
mod ping_pong;
mod ready;
mod settings;
mod state;

pub use self::connection::{Connection};
pub use self::framed_read::FramedRead;
pub use self::framed_write::FramedWrite;
pub use self::handshake::Handshake;
pub use self::ping_pong::PingPong;
pub use self::ready::ReadySink;
pub use self::settings::Settings;
pub use self::state::State;

use tokio_io::codec::length_delimited;

/// Base HTTP/2.0 transport. Only handles framing.
type Framed<T> =
    FramedWrite<
        FramedRead<
            length_delimited::FramedRead<T>>>;

type Inner<T> =
    Settings<
        PingPong<
            Framed<T>>>;
