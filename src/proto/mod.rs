use {frame, Peer, StreamId};
use error::Reason;
use frame::Frame;

use bytes::{Buf, IntoBuf};
use futures::*;
use tokio_io::{AsyncRead, AsyncWrite};
use tokio_io::codec::length_delimited;

macro_rules! proxy_stream {
    ($struct:ident $(, $targs:ident)*) => (
        impl<T: Stream$(, $targs)*> Stream for $struct<T$(, $targs)*> {
            type Item = T::Item;
            type Error = T::Error;
            fn poll(&mut self) -> Poll<Option<T::Item>, T::Error> {
                self.inner.poll()
            }
        }
    )
}

macro_rules! proxy_sink {
    ($struct:ident $(, $targs:ident)*) => (
        impl<T, U$(, $targs)*> Sink for $struct<T$(, $targs)*>
            where T: Sink<SinkItem = frame::Frame<U>, SinkError = ConnectionError>
        {
            type SinkItem = frame::Frame<U>;
            type SinkError = ConnectionError;
            fn start_send(&mut self, it: T::SinkItem) -> StartSend<T::SinkItem, T::SinkError> {
                self.inner.start_send(it)
            }
            fn poll_complete(&mut self) -> Poll<(), T::SinkError> {
                self.inner.poll_complete()
            }
        }
    )
}

macro_rules! proxy_ready_sink {
    ($struct:ident $(, $targs:ident)*$(; $constraint:ident)*) => (
        impl<T, U$(, $targs)*> ReadySink for $struct<T$(, $targs)*>
            where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
                  T: ReadySink $(+ $constraint)*
        {
            fn poll_ready(&mut self) -> Poll<(), T::SinkError> {
                self.inner.poll_ready()
            }
        }
    )
}

// First, pull in the internal interfaces that support macros used throughout this module.

#[macro_use]
mod apply_settings;
#[macro_use]
mod control_flow;
#[macro_use]
mod control_ping;
mod control_settings;
#[macro_use]
mod control_streams;

use self::apply_settings::ApplySettings;
use self::control_flow::{ControlFlowRecv, ControlFlowSend};
use self::control_ping::ControlPing;
use self::control_settings::ControlSettings;
use self::control_streams::ControlStreams;

mod connection;
mod flow_control_recv;
mod flow_control_send;
mod flow_control_state;
mod framed_read;
mod framed_write;
mod ping_pong;
mod ready;
mod settings;
mod stream_recv_close;
mod stream_recv_open;
mod stream_send_close;
mod stream_send_open;
mod stream_state;
mod stream_states;

pub use self::connection::Connection;

use self::flow_control_recv::FlowControlRecv;
use self::flow_control_send::FlowControlSend;
use self::flow_control_state::FlowControlState;
use self::framed_read::FramedRead;
use self::framed_write::FramedWrite;
use self::ping_pong::PingPong;
use self::ready::ReadySink;
use self::settings::Settings;
use self::stream_recv_close::StreamRecvClose;
use self::stream_recv_open::StreamRecvOpen;
use self::stream_send_close::StreamSendClose;
use self::stream_send_open::StreamSendOpen;
use self::stream_states::StreamStates;

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
/// ### The stream transport
///
/// The states of all HTTP/2 connections are stored centrally in the `StreamStates` at the
/// bottom of the stream transport. Several modules above this access this state via the
/// `ControlStreams` API to drive changes to the stream state.  In each direction (send
/// from local to remote, and recv from remote to local), there is an Stream\*Open module
/// responsible for initializing new streams and ensuring that frames do not violate
/// stream state. Then, there are modules that operate on streams (for instance,
/// FlowControl).  Finally, a Stream\*Close module is responsible for acting on END_STREAM
/// frames to ensure that stream states are not closed before work is complete.
///
/// #### `StreamSendOpen`
///
/// - Initializes streams initiated by the local peer.
/// - Ensures that frames sent from the local peer are appropriate for the stream's state.
/// - Ensures that the remote's max stream concurrency is not violated.
///
/// #### `FlowControlSend`
///
/// - Tracks sent data frames against the remote stream and connection flow control
///   windows.
/// - Tracks remote settings updates to SETTINGS_INITIAL_WINDOW_SIZE.
/// - Exposes `ControlFlowSend` upwards.
///   - Tracks received window updates against the remote stream and connection flow
///     control windows so that upper layers may poll for updates.
///
/// #### `StreamSendClose`
///
/// - Updates the stream state for frames sent with END_STREAM.
///
/// #### `StreamRecvClose`
///
/// - Updates the stream state for frames received with END_STREAM.
///
/// #### `FlowControlRecv`
///
/// - Tracks received data frames against the local stream and connection flow control
///   windows.
/// - Tracks remote settings updates to SETTINGS_INITIAL_WINDOW_SIZE.
/// - Exposes `ControlFlowRecv` upwards.
///   - Sends window updates for the local stream and connection flow control windows as
///     instructed by upper layers.
///
/// #### `StreamRecvOpen`
///
/// - Initializes streams initiated by the remote peer.
/// - Ensures that frames received from the remote peer are appropriate for the stream's
///   state.
/// - Ensures that the local peer's max stream concurrency is not violated.
///   - Emits StreamRefused resets to the remote.
///
/// #### `StreamStates`
///
/// - Holds the state of all local & remote active streams.
/// - Holds the cause of all reset/closed streams.
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
        FlowControlSend<
            StreamSendClose<
                StreamRecvClose<
                    FlowControlRecv<
                        StreamRecvOpen<
                            StreamStates<T, P>>>>>>>;

type Codec<T, B> =
    FramedRead<
        FramedWrite<T, B>>;

pub type PingPayload = [u8; 8];

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
pub fn from_io<T, P, B>(io: T, local_settings: frame::SettingSet)
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
    let transport = Settings::new(framed_write, local_settings.clone());

    from_server_handshaker(transport, local_settings)
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
pub fn from_server_handshaker<T, P, B>(settings: Settings<FramedWrite<T, B::Buf>>,
                                       local_settings: frame::SettingSet)
    -> Connection<T, P, B>
    where T: AsyncRead + AsyncWrite,
          P: Peer,
          B: IntoBuf,
{
    let initial_recv_window_size = local_settings.initial_window_size().unwrap_or(65_535);
    let local_max_concurrency = local_settings.max_concurrent_streams();

    let initial_send_window_size = settings.remote_initial_window_size();
    let remote_max_concurrency = settings.remote_max_concurrent_streams();

    // Replace Settings' writer with a full transport.
    let transport = settings.swap_inner(|io| {
        // Delimit the frames.
        let framed = length_delimited::Builder::new()
            .big_endian()
            .length_field_length(3)
            .length_adjustment(9)
            .num_skip(0) // Don't skip the header
            .new_read(io);

        trace!("composing transport");
        StreamSendOpen::new(
            initial_send_window_size,
            remote_max_concurrency,
            FlowControlSend::new(
                initial_send_window_size,
                StreamSendClose::new(
                    StreamRecvClose::new(
                        FlowControlRecv::new(
                            initial_recv_window_size,
                            StreamRecvOpen::new(
                                initial_recv_window_size,
                                local_max_concurrency,
                                StreamStates::new(
                                    PingPong::new(
                                        FramedRead::new(framed)))))))))
    });

    connection::new(transport)
}

