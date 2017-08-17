use {frame, ConnectionError};
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

    /// List of streams waiting for outbound connection capacity
    pending_capacity: store::List<B>,

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
            pending_capacity: store::List::new(),
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
                        stream: &mut store::Ptr<B>)
        -> Result<(), ConnectionError>
    {
        trace!("send_headers; frame={:?}; init_window={:?}", frame, self.init_window_sz);
        // Update the state
        stream.state.send_open(frame.is_end_stream())?;

        if stream.state.is_send_streaming() {
            stream.send_flow.set_window_size(self.init_window_sz);
        }

        // Queue the frame for sending
        self.prioritize.queue_frame(frame.into(), stream);

        Ok(())
    }

    pub fn send_eos(&mut self, stream: &mut Stream<B>)
        -> Result<(), ConnectionError>
    {
        stream.state.send_close()
    }

    pub fn send_data(&mut self,
                     frame: frame::Data<B>,
                     stream: &mut store::Ptr<B>)
        -> Result<(), ConnectionError>
    {
        let sz = frame.payload().remaining();

        if sz > MAX_WINDOW_SIZE as usize {
            // TODO: handle overflow
            unimplemented!();
        }

        let sz = sz as WindowSize;

        if !stream.state.is_send_streaming() {
            if stream.state.is_closed() {
                return Err(InactiveStreamId.into());
            } else {
                return Err(UnexpectedFrameType.into());
            }
        }

        // Update the flow controller
        stream.send_flow.send(sz, FlowControlViolation)?;

        if frame.is_end_stream() {
            try!(stream.state.send_close());
        }

        self.prioritize.queue_frame(frame.into(), stream);

        Ok(())
    }

    pub fn poll_complete<T>(&mut self,
                            store: &mut Store<B>,
                            dst: &mut Codec<T, Prioritized<B>>)
        -> Poll<(), ConnectionError>
        where T: AsyncWrite,
    {
        self.prioritize.poll_complete(store, dst)
    }

    pub fn recv_connection_window_update(&mut self,
                                         frame: frame::WindowUpdate,
                                         store: &mut Store<B>)
        -> Result<(), ConnectionError>
    {
        self.prioritize.recv_window_update(frame)?;

        // Get the current connection capacity
        let connection = self.prioritize.available_window();

        // Walk each stream pending capacity and see if this change to the
        // connection window can increase the advertised capacity of the stream.
        //
        // TODO: This is not a hugely efficient operation. It could be better to
        // change the pending_capacity structure to a red-black tree.
        //
        self.pending_capacity.retain::<stream::NextCapacity, _>(
            store,
            |stream| {
                // Make sure that the stream is flagged as queued
                debug_assert!(stream.is_pending_send_capacity);

                if stream.send_flow.is_fully_advertised() || !stream.state.is_send_streaming() {
                    stream.is_pending_send_capacity = false;
                    return false;
                }

                if connection <= stream.send_flow.advertised() {
                    // The window is not increased, but we remain interested in
                    // updates in the future.
                    return true;
                }


                stream.send_flow.advertise(connection);
                stream.notify_send();

                true
            });

        Ok(())
    }

    pub fn recv_stream_window_update(&mut self,
                                     frame: frame::WindowUpdate,
                                     stream: &mut store::Ptr<B>)
        -> Result<(), ConnectionError>
    {
        if !stream.state.is_send_streaming() {
            return Ok(());
        }

        let connection = self.prioritize.available_window();
        let advertised = stream.send_flow.advertised();

        stream.send_flow.expand_window(frame.size_increment())?;
        stream.send_flow.advertise(connection);

        if !stream.is_pending_send_capacity && !stream.send_flow.is_fully_advertised() {
            // The stream transitioned to having unadvertised capacity
            stream.is_pending_send_capacity = true;
            self.pending_capacity.push::<stream::NextCapacity>(stream);
        }

        if advertised < stream.send_flow.advertised() {
            // The amount of available capacity to the sender has increased,
            // notify the sender
            stream.notify_send();
        }

        Ok(())
    }

    pub fn window_size(&mut self, stream: &mut Stream<B>) -> usize {
        if stream.state.is_send_streaming() {
            // Track the current task
            stream.send_task = Some(task::current());

            stream.send_flow.observe_window()
        } else {
            0
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

                    unimplemented!();
                    /*
                    if let Some(flow) = stream.state.send_flow_control() {
                        flow.shrink_window(val);

                        // Update the unadvertised number as well
                        if stream.unadvertised_send_window < dec {
                            stream.unadvertised_send_window = 0;
                        } else {
                            stream.unadvertised_send_window -= dec;
                        }

                        unimplemented!();
                    }
                    */
                });
            } else if val > old_val {
                let inc = val - old_val;

                store.for_each(|mut stream| {
                    unimplemented!();
                    /*
                    if let Some(flow) = stream.state.send_flow_control() {
                        unimplemented!();
                    }
                    */
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
