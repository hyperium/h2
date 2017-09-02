mod connection;
mod error;
mod peer;
mod ping_pong;
mod settings;
mod streams;

pub(crate) use self::connection::Connection;
pub(crate) use self::error::Error;
pub(crate) use self::peer::Peer;
pub(crate) use self::streams::{Streams, StreamRef};

use codec::Codec;

use self::ping_pong::PingPong;
use self::settings::Settings;
use self::streams::Prioritized;

use frame::{self, Frame};

use futures::{task, Poll, Async};
use futures::task::Task;

use bytes::Buf;

use tokio_io::AsyncWrite;

pub type PingPayload = [u8; 8];

pub type WindowSize = u32;

// Constants
// TODO: Move these into `frame`
pub const DEFAULT_INITIAL_WINDOW_SIZE: WindowSize = 65_535;
pub const MAX_WINDOW_SIZE: WindowSize = (1 << 31) - 1;
