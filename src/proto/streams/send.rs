use {frame, ConnectionError};
use error::User::InactiveStreamId;
use proto::*;
use super::*;

use error::User::*;

use bytes::Buf;

use std::collections::VecDeque;
use std::marker::PhantomData;

/// Manages state transitions related to outbound frames.
#[derive(Debug)]
pub(super) struct Send<B> {
    /// Maximum number of locally initiated streams
    max_streams: Option<usize>,

    /// Current number of locally initiated streams
    num_streams: usize,

    /// Stream identifier to use for next initialized stream.
    next_stream_id: StreamId,

    /// Initial window size of locally initiated streams
    init_window_sz: WindowSize,

    /// Task awaiting notification to open a new stream.
    blocked_open: Option<task::Task>,

    /// Prioritization layer
    prioritize: Prioritize<B>,
}

impl<B> Send<B> where B: Buf {
    /// Create a new `Send`
    pub fn new<P: Peer>(config: &Config) -> Self {
        let next_stream_id = if P::is_server() { 2 } else { 1 };

        Send {
            max_streams: config.max_local_initiated,
            num_streams: 0,
            next_stream_id: next_stream_id.into(),
            init_window_sz: config.init_local_window_sz,
            blocked_open: None,
            prioritize: Prioritize::new(config),
        }
    }

    pub fn poll_open_ready<P: Peer>(&mut self) -> Poll<(), ConnectionError> {
        try!(self.ensure_can_open::<P>());

        if let Some(max) = self.max_streams {
            if max <= self.num_streams {
                self.blocked_open = Some(task::current());
                return Ok(Async::NotReady);
            }
        }

        return Ok(Async::Ready(()));
    }

    /// Update state reflecting a new, locally opened stream
    ///
    /// Returns the stream state if successful. `None` if refused
    pub fn open<P: Peer>(&mut self)
        -> Result<Stream<B>, ConnectionError>
    {
        try!(self.ensure_can_open::<P>());

        if let Some(max) = self.max_streams {
            if max <= self.num_streams {
                return Err(Rejected.into());
            }
        }

        let ret = Stream::new(self.next_stream_id);

        // Increment the number of locally initiated streams
        self.num_streams += 1;
        self.next_stream_id.increment();

        Ok(ret)
    }

    pub fn send_headers(&mut self,
                        frame: frame::Headers,
                        stream: &mut store::Ptr<B>,
                        task: &mut Option<Task>)
        -> Result<(), ConnectionError>
    {
        trace!("send_headers; frame={:?}; init_window={:?}", frame, self.init_window_sz);
        // Update the state
        stream.state.send_open(frame.is_end_stream())?;

        if stream.state.is_send_streaming() {
            stream.send_flow.inc_window(self.init_window_sz)?;
        }

        // Queue the frame for sending
        self.prioritize.queue_frame(frame.into(), stream, task);

        Ok(())
    }

    pub fn send_eos(&mut self, stream: &mut Stream<B>)
        -> Result<(), ConnectionError>
    {
        stream.state.send_close()
    }

    pub fn send_reset(&mut self, reason: Reason,
                      stream: &mut store::Ptr<B>,
                      task: &mut Option<Task>)
        -> Result<(), ConnectionError>
    {
        if stream.state.is_closed() {
            return Err(InactiveStreamId.into())
        }

        stream.state.send_reset(reason)?;

        let frame = frame::Reset::new(stream.id, reason);
        self.prioritize.queue_frame(frame.into(), stream, task);

        Ok(())
    }

    pub fn send_data(&mut self,
                     frame: frame::Data<B>,
                     stream: &mut store::Ptr<B>,
                     task: &mut Option<Task>)
        -> Result<(), ConnectionError>
    {
        self.prioritize.send_data(frame, stream, task)
    }

    pub fn poll_complete<T>(&mut self,
                            store: &mut Store<B>,
                            dst: &mut Codec<T, Prioritized<B>>)
        -> Poll<(), ConnectionError>
        where T: AsyncWrite,
    {
        self.prioritize.poll_complete(store, dst)
    }

    /// Request capacity to send data
    pub fn reserve_capacity(&mut self, capacity: WindowSize, stream: &mut store::Ptr<B>)
        -> Result<(), ConnectionError>
    {
        self.prioritize.reserve_capacity(capacity, stream)
    }

    pub fn poll_capacity(&mut self, stream: &mut store::Ptr<B>)
        -> Poll<Option<WindowSize>, ConnectionError>
    {
        if !stream.state.is_send_streaming() {
            return Ok(Async::Ready(None));
        }

        if !stream.send_capacity_inc {
            return Ok(Async::NotReady);
        }

        stream.send_capacity_inc = false;

        Ok(Async::Ready(Some(self.capacity(stream))))
    }

    /// Current available stream send capacity
    pub fn capacity(&self, stream: &mut store::Ptr<B>) -> WindowSize {
        let available = stream.send_flow.available();
        let buffered = stream.buffered_send_data;

        if available <= buffered {
            0
        } else {
            available - buffered
        }
    }

    pub fn recv_connection_window_update(&mut self,
                                         frame: frame::WindowUpdate,
                                         store: &mut Store<B>)
        -> Result<(), ConnectionError>
    {
        self.prioritize.recv_connection_window_update(frame.size_increment(), store)
    }

    pub fn recv_stream_window_update(&mut self,
                                     sz: WindowSize,
                                     stream: &mut store::Ptr<B>)
    {
        if let Err(e) = self.prioritize.recv_stream_window_update(sz, stream) {
            // TODO: Send reset
            unimplemented!();
        }
    }

    pub fn apply_remote_settings(&mut self,
                                 settings: &frame::Settings,
                                 store: &mut Store<B>)
    {
        if let Some(val) = settings.max_concurrent_streams() {
            self.max_streams = Some(val as usize);
        }

        // Applies an update to the remote endpoint's initial window size.
        //
        // Per RFC 7540 §6.9.2:
        //
        // In addition to changing the flow-control window for streams that are
        // not yet active, a SETTINGS frame can alter the initial flow-control
        // window size for streams with active flow-control windows (that is,
        // streams in the "open" or "half-closed (remote)" state). When the
        // value of SETTINGS_INITIAL_WINDOW_SIZE changes, a receiver MUST adjust
        // the size of all stream flow-control windows that it maintains by the
        // difference between the new value and the old value.
        //
        // A change to `SETTINGS_INITIAL_WINDOW_SIZE` can cause the available
        // space in a flow-control window to become negative. A sender MUST
        // track the negative flow-control window and MUST NOT send new
        // flow-controlled frames until it receives WINDOW_UPDATE frames that
        // cause the flow-control window to become positive.
        if let Some(val) = settings.initial_window_size() {
            let old_val = self.init_window_sz;
            self.init_window_sz = val;

            if val < old_val {
                let dec = old_val - val;

                store.for_each(|mut stream| {
                    let stream = &mut *stream;

                    if stream.state.is_send_streaming() {
                        stream.send_flow.dec_window(dec);

                        // TODO: Handle reclaiming connection level window
                        // capacity.

                        // TODO: Should this notify the producer?
                    }
                });
            } else if val > old_val {
                let inc = val - old_val;

                store.for_each(|mut stream| {
                    self.recv_stream_window_update(inc, &mut stream);
                });
            }
        }
    }

    pub fn ensure_not_idle(&self, id: StreamId) -> Result<(), ConnectionError> {
        if id >= self.next_stream_id {
            return Err(ProtocolError.into());
        }

        Ok(())
    }

    pub fn dec_num_streams(&mut self) {
        self.num_streams -= 1;

        if self.num_streams < self.max_streams.unwrap_or(::std::usize::MAX) {
            if let Some(task) = self.blocked_open.take() {
                task.notify();
            }
        }
    }

    /// Returns true if the local actor can initiate a stream with the given ID.
    fn ensure_can_open<P: Peer>(&self) -> Result<(), ConnectionError> {
        if P::is_server() {
            // Servers cannot open streams. PushPromise must first be reserved.
            return Err(UnexpectedFrameType.into());
        }

        // TODO: Handle StreamId overflow

        Ok(())
    }
}
