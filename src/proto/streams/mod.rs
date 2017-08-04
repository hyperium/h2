mod buffer;
mod flow_control;
mod prioritize;
mod recv;
mod send;
mod state;
mod store;
mod stream;
mod streams;

pub use self::streams::{Streams, StreamRef};

use self::buffer::Buffer;
use self::flow_control::FlowControl;
use self::prioritize::Prioritize;
use self::recv::Recv;
use self::send::Send;
use self::state::State;
use self::store::{Store, Entry};
use self::stream::Stream;

use {frame, StreamId, ConnectionError};
use proto::*;
use error::Reason::*;
use error::User::*;

use http::{Request, Response};
use bytes::Bytes;

#[derive(Debug)]
pub struct Config {
    /// Maximum number of remote initiated streams
    pub max_remote_initiated: Option<usize>,

    /// Initial window size of remote initiated streams
    pub init_remote_window_sz: WindowSize,

    /// Maximum number of locally initiated streams
    pub max_local_initiated: Option<usize>,

    /// Initial window size of locally initiated streams
    pub init_local_window_sz: WindowSize,
}
