mod buffer;
mod counts;
mod flow_control;
mod prioritize;
mod recv;
mod send;
mod state;
mod store;
mod stream;
mod streams;

pub(crate) use self::prioritize::Prioritized;
pub(crate) use self::store::Key;
pub(crate) use self::streams::{StreamRef, Streams};

use self::buffer::Buffer;
use self::counts::Counts;
use self::flow_control::FlowControl;
use self::prioritize::Prioritize;
use self::recv::Recv;
use self::send::Send;
use self::state::State;
use self::store::{Entry, Store};
use self::stream::Stream;

use error::Reason::*;
use frame::{StreamId, StreamIdOverflow};
use proto::*;

use bytes::Bytes;
use http::{Request, Response};

#[derive(Debug)]
pub struct Config {
    /// Initial window size of locally initiated streams
    pub local_init_window_sz: WindowSize,

    /// Maximum number of locally initiated streams
    pub local_max_initiated: Option<usize>,

    /// The stream ID to start the next local stream with
    pub local_next_stream_id: StreamId,

    /// If the local peer is willing to receive push promises
    pub local_push_enabled: bool,

    /// Initial window size of remote initiated streams
    pub remote_init_window_sz: WindowSize,

    /// Maximum number of remote initiated streams
    pub remote_max_initiated: Option<usize>,
}
