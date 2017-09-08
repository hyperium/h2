mod connection;
mod error;
mod peer;
mod ping_pong;
mod settings;
mod streams;

pub(crate) use self::connection::Connection;
pub(crate) use self::error::Error;
pub(crate) use self::peer::Peer;
pub(crate) use self::streams::{StreamRef, Streams};

use codec::Codec;

use self::ping_pong::PingPong;
use self::settings::Settings;
use self::streams::Prioritized;

use frame::{self, Frame};

use futures::{task, Async, Poll};
use futures::task::Task;

use bytes::Buf;

use tokio_io::AsyncWrite;

pub type PingPayload = [u8; 8];

pub type WindowSize = u32;

// Constants
pub const MAX_WINDOW_SIZE: WindowSize = (1 << 31) - 1;
