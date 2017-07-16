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
pub use self::flow_controller::{FlowController, WindowUnderflow};
pub use self::framed_read::FramedRead;
pub use self::framed_write::FramedWrite;
pub use self::ping_pong::PingPong;
pub use self::ready::ReadySink;
pub use self::settings::Settings;
pub use self::stream_tracker::StreamTracker;
use self::state::StreamState;

use {frame, ConnectionError, Peer, StreamId};

use tokio_io::{AsyncRead, AsyncWrite};
use tokio_io::codec::length_delimited;

use bytes::{Buf, IntoBuf};

use ordermap::{Entry, OrderMap};
use fnv::FnvHasher;
use std::hash::BuildHasherDefault;

/// Represents the internals of an HTTP2 connection.
///
/// A transport consists of several layers (_transporters_) and is arranged from _top_
/// (near the application) to _bottom_ (near the network).  Each transporter implements a
/// Stream of frames received from the remote, and a ReadySink of frames sent to the
/// remote.
///
/// At the top of the transport, the Settings module is responsible for:
/// - Transmitting local settings to the remote.
/// - Sending settings acknowledgements for all settings frames received from the remote.
/// - Exposing settings upward to the Connection.
///
/// All transporters below Settings must apply relevant settings before passing a frame on
/// to another level.  For example, if the frame writer n
type Transport<T, P, B> =
    Settings<
        FlowControl<
            StreamTracker<
                PingPong<
                    Framer<T, B>,
                    B>,
                P>>>;

type Framer<T, B> =
    FramedRead<
        FramedWrite<T, B>>;

pub type WindowSize = u32;

#[derive(Debug, Default)]
pub struct StreamMap {
    inner: OrderMap<StreamId, StreamState, BuildHasherDefault<FnvHasher>>
}

impl StreamMap {
    fn get_mut(&mut self, id: &StreamId) -> Option<&mut StreamState> {
        self.inner.get_mut(id)
    }

    fn entry(&mut self, id: StreamId) -> Entry<StreamId, StreamState, BuildHasherDefault<FnvHasher>> {
        self.inner.entry(id)
    }

    fn shrink_all_local_windows(&mut self, decr: u32) {
        for (_, mut s) in &mut self.inner {
            s.shrink_local_window(decr)
        }
    }

    fn grow_all_local_windows(&mut self, incr: u32) {
        for (_, mut s) in &mut self.inner {
            s.grow_local_window(incr)
        }
    }
    
    fn shrink_all_remote_windows(&mut self, decr: u32) {
        for (_, mut s) in &mut self.inner {
            s.shrink_remote_window(decr)
        }
    }

    fn grow_all_remote_windows(&mut self, incr: u32) {
        for (_, mut s) in &mut self.inner {
            s.grow_remote_window(incr)
        }
    }
}

/// Allows settings to be applied from the top of the stack to the lower levels.d
pub trait ConnectionTransporter {
    fn apply_local_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError>;
    fn apply_remote_settings(&mut self, set: &frame::SettingSet) -> Result<(), ConnectionError>;
}

pub trait StreamTransporter {
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
    //
    // We first create a Settings directly around a framed writer
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
pub fn from_server_handshaker<T, P, B>(settings: Settings<FramedWrite<T, B::Buf>>)
    -> Connection<T, P, B>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
          B: IntoBuf,
{
    let initial_local_window_size = settings.local_settings().initial_window_size();
    let initial_remote_window_size = settings.remote_settings().initial_window_size();
    let local_max_concurrency = settings.local_settings().max_concurrent_streams();
    let remote_max_concurrency = settings.remote_settings().max_concurrent_streams();

    // Replace Settings' writer with a full transport.
    let transport = settings.swap_inner(|io| {
        // Delimit the frames.
        let framer = length_delimited::Builder::new()
            .big_endian()
            .length_field_length(3)
            .length_adjustment(9)
            .num_skip(0) // Don't skip the header
            .new_read(io);

        FlowControl::new(
            initial_local_window_size,
            initial_remote_window_size,
            StreamTracker::new(
                initial_local_window_size,
                initial_remote_window_size,
                local_max_concurrency,
                remote_max_concurrency,
                PingPong::new(FramedRead::new(framer))
            )
        )
    });

    connection::new(transport)
}
