mod connection;
mod framed_read;
mod framed_write;
mod ping_pong;
mod ready;
mod settings;
mod state;

pub use self::connection::{Connection};
pub use self::framed_read::FramedRead;
pub use self::framed_write::FramedWrite;
pub use self::ping_pong::PingPong;
pub use self::ready::ReadySink;
pub use self::settings::Settings;
pub use self::state::State;

use {frame, Peer};

use tokio_io::{AsyncRead, AsyncWrite};
use tokio_io::codec::length_delimited;

type Inner<T> =
    Settings<
        PingPong<
            Framed<T>>>;

type Framed<T> =
    FramedRead<
        FramedWrite<T>>;

pub fn from_io<T, P>(io: T, settings: frame::SettingSet)
    -> Connection<T, P>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
{
    let framed_write = FramedWrite::new(io);

    // Delimit the frames
    let framed_read = length_delimited::Builder::new()
        .big_endian()
        .length_field_length(3)
        .length_adjustment(9)
        .num_skip(0) // Don't skip the header
        .new_read(framed_write);

    // Map to `Frame` types
    let framed = FramedRead::new(framed_read);

    // Add ping/pong responder.
    let ping_pong = PingPong::new(framed);

    // Add settings handler
    let settings = Settings::new(
        ping_pong, settings);

    // Finally, return the constructed `Connection`
    connection::new(settings)
}
