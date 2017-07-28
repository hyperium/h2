
mod connection;
mod flow_control;
mod framed_read;
mod framed_write;
mod ping_pong;
mod settings;
mod state;
mod streams;

pub use self::connection::Connection;

use self::flow_control::FlowControl;
use self::framed_read::FramedRead;
use self::framed_write::FramedWrite;
use self::ping_pong::PingPong;
use self::settings::Settings;
use self::streams::Streams;

use error::{Reason, ConnectionError};
use frame::Frame;

use futures::*;
use bytes::{Buf};
use tokio_io::{AsyncRead, AsyncWrite};

pub type PingPayload = [u8; 8];

pub type WindowSize = u32;

#[derive(Debug)]
pub struct WindowUpdate;

type Codec<T, B> =
    FramedRead<
        FramedWrite<T, B>>;

// Constants
pub const DEFAULT_INITIAL_WINDOW_SIZE: WindowSize = 65_535;
