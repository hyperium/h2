mod connection;
mod flow_control;
mod framed_read;
mod framed_write;
mod ping_pong;
mod ready;
mod settings;
mod state;
mod window_update;

pub use self::connection::Connection;
pub use self::flow_control::FlowController;
pub use self::framed_read::FramedRead;
pub use self::framed_write::FramedWrite;
pub use self::ping_pong::PingPong;
pub use self::ready::ReadySink;
pub use self::settings::Settings;
pub use self::state::{PeerState, State};
pub use self::window_update::WindowUpdate;

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

/// Create a full H2 transport from an I/O handle.
///
/// This is called as the final step of the client handshake future.
pub fn from_io<T, P>(io: T, settings: frame::SettingSet)
    -> Connection<T, P>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
{
    let framed_write = FramedWrite::new(io);

    // To avoid code duplication, we're going to go this route. It is a bit
    // weird, but oh well...
    let settings = Settings::new(
        framed_write, settings);

    from_server_handshaker(settings)
}

/// Create a transport prepared to handle the server handshake.
///
/// When the server is performing the handshake, it is able to only send
/// `Settings` frames and is expected to receive the client preface as a byte
/// stream. To represent this, `Settings<FramedWrite<T>>` is returned.
pub fn server_handshaker<T>(io: T, settings: frame::SettingSet)
    -> Settings<FramedWrite<T>>
    where T: AsyncRead + AsyncWrite,
{
    let framed_write = FramedWrite::new(io);

    Settings::new(framed_write, settings)
}

/// Create a full H2 transport from the server handshaker
pub fn from_server_handshaker<T, P>(transport: Settings<FramedWrite<T>>)
    -> Connection<T, P>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
{
    let settings = transport.swap_inner(|io| {
        // Delimit the frames
        let framed_read = length_delimited::Builder::new()
            .big_endian()
            .length_field_length(3)
            .length_adjustment(9)
            .num_skip(0) // Don't skip the header
            .new_read(io);

        // Map to `Frame` types
        let framed = FramedRead::new(framed_read);

        // Add ping/pong responder.
        PingPong::new(framed)
    });

    // Finally, return the constructed `Connection`
    connection::new(settings, 65_535, 65_535)
}
