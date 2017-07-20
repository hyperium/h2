use {frame, ConnectionError, Peer, StreamId};
use error::Reason;
use frame::{Frame, SettingSet};

use bytes::{Buf, IntoBuf};
use futures::*;
use tokio_io::{AsyncRead, AsyncWrite};
use tokio_io::codec::length_delimited;

mod connection;
mod flow_control;
mod flow_control_state;
mod framed_read;
mod framed_write;
mod ping_pong;
mod ready;
mod settings;
mod state;
mod stream_recv_close;
mod stream_recv_open;
mod stream_send_close;
mod stream_send_open;
mod stream_store;

pub use self::connection::Connection;

use self::flow_control::{ControlFlow, FlowControl};
use self::flow_control_state::{FlowControlState, WindowUnderflow};
use self::framed_read::FramedRead;
use self::framed_write::FramedWrite;
use self::ping_pong::{ControlPing, PingPayload, PingPong};
use self::ready::ReadySink;
use self::settings::{ApplySettings, ControlSettings, Settings};
use self::state::{StreamState, PeerState};
use self::stream_recv_close::StreamRecvClose;
use self::stream_recv_open::StreamRecvOpen;
use self::stream_send_close::StreamSendClose;
use self::stream_send_open::StreamSendOpen;
use self::stream_store::{ControlStreams, StreamStore};

/// Represents the internals of an HTTP/2 connection.
///
/// A transport consists of several layers (_transporters_) and is arranged from _top_
/// (near the application) to _bottom_ (near the network).  Each transporter implements a
/// Stream of frames received from the remote, and a ReadySink of frames sent to the
/// remote.
///
/// ## Transport Layers
///
/// ### `Settings`
///
/// - Receives remote settings frames and applies the settings downward through the
///   transport (via the ApplySettings trait) before responding with acknowledgements.
/// - Exposes ControlSettings up towards the application and transmits local settings to
///   the remote.
///
/// ### `FlowControl`
///
/// - Tracks received data frames against the local stream and connection flow control
///   windows.
/// - Tracks sent data frames against the remote stream and connection flow control
///   windows.
/// - Tracks remote settings updates to SETTINGS_INITIAL_WINDOW_SIZE.
/// - Exposes `ControlFlow` upwards.
///   - Tracks received window updates against the remote stream and connection flow
///     control windows so that upper layers may poll for updates.
///   - Sends window updates for the local stream and connection flow control windows as
///     instructed by upper layers.
///
/// ### `StreamTracker`
///
/// - Tracks all active streams.
/// - Tracks all reset streams.
/// - Exposes `ControlStreams` so that upper layers may share stream state.
///
/// ### `PingPong`
///
/// - Acknowleges PINGs from the remote.
/// - Exposes ControlPing that allows the application side to send ping requests to the
///   remote. Acknowledgements from the remoe are queued to be consumed by the
///   application.
///
/// ### FramedRead
///
/// - Decodes frames from bytes.
///
/// ### FramedWrite
///
/// - Encodes frames to bytes.
///
type Transport<T, P, B>=
    Settings<
        Streams<
            PingPong<
                Codec<T, B>,
                B>,
            P>>;

type Streams<T, P> =
    StreamSendOpen<
        StreamRecvClose<
            FlowControl<
                StreamSendClose<
                    StreamRecvOpen<
                        StreamStore<T, P>>>>>>;

type Codec<T, B> =
    FramedRead<
        FramedWrite<T, B>>;

pub type WindowSize = u32;

#[derive(Debug, Copy, Clone)]
pub struct WindowUpdate {
    stream_id: StreamId,
    increment: WindowSize
}

impl WindowUpdate {
    pub fn new(stream_id: StreamId, increment: WindowSize) -> WindowUpdate {
        WindowUpdate { stream_id, increment }
    }

    pub fn stream_id(&self) -> StreamId {
        self.stream_id
    }

    pub fn increment(&self) -> WindowSize {
        self.increment
    }
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
    let transport = Settings::new(
        framed_write, settings);

    from_server_handshaker(transport)
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
        let framed = length_delimited::Builder::new()
            .big_endian()
            .length_field_length(3)
            .length_adjustment(9)
            .num_skip(0) // Don't skip the header
            .new_read(io);

        StreamSendOpen::new(
            initial_remote_window_size,
            remote_max_concurrency,
            StreamRecvClose::new(
                FlowControl::new(
                    initial_local_window_size,
                    initial_remote_window_size,
                    StreamSendClose::new(
                        StreamRecvOpen::new(
                            initial_local_window_size,
                            local_max_concurrency,
                            StreamStore::new(
                                PingPong::new(
                                    FramedRead::new(framed))))))))
    });

    connection::new(transport)
}
