mod connection;
mod flow_control;
mod flow_controller;
mod framed_read;
mod framed_write;
mod ping_pong;
mod ready;
mod settings;
mod state;
mod stream_tracker;

pub use self::connection::Connection;
pub use self::flow_control::FlowControl;
pub use self::flow_controller::FlowController;
pub use self::framed_read::FramedRead;
pub use self::framed_write::FramedWrite;
pub use self::ping_pong::PingPong;
pub use self::ready::ReadySink;
pub use self::settings::Settings;
pub use self::stream_tracker::StreamTracker;
use self::state::StreamState;

use {frame, Peer, StreamId};

use tokio_io::{AsyncRead, AsyncWrite};
use tokio_io::codec::length_delimited;

use bytes::{Buf, IntoBuf};

use ordermap::OrderMap;
use fnv::FnvHasher;
use std::hash::BuildHasherDefault;

/// Represents
type Transport<T, B> =
    Settings<
        FlowControl<
            StreamTracker<
                PingPong<
                    Framer<T, B>,
                    B>>>>;

type Framer<T, B> =
    FramedRead<
        FramedWrite<T, B>>;


pub type WindowSize = u32;

#[derive(Debug)]
struct StreamMap {
    inner: OrderMap<StreamId, StreamState, BuildHasherDefault<FnvHasher>>
}

trait StreamTransporter {
    fn streams(&self)-> &StreamMap;
    fn streams_mut(&mut self) -> &mut StreamMap;
}

/// Create a full H2 transport from an I/O handle.
///
/// This is called as the final step of the client handshake future.
pub fn from_io<T, P, B>(io: T, settings: frame::SettingSet)
    -> Connection<T, P, B>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
          B: IntoBuf,
{
    let framed_write: FramedWrite<_, B::Buf> = FramedWrite::new(io);

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
pub fn server_handshaker<T, B>(io: T, settings: frame::SettingSet)
    -> Settings<FramedWrite<T, B>>
    where T: AsyncRead + AsyncWrite,
          B: Buf,
{
    let framed_write = FramedWrite::new(io);

    Settings::new(framed_write, settings)
}

/// Create a full H2 transport from the server handshaker
pub fn from_server_handshaker<T, P, B>(transport: Settings<FramedWrite<T, B::Buf>>)
    -> Connection<T, P, B>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
          B: IntoBuf,
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

        FlowControl::new(
            StreamTracker::new(
                PingPong::new(
                    framed)))
    });

    // Finally, return the constructed `Connection`
    connection::new(settings)
}
