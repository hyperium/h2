use super::*;
use codec::{RecvError, UserError};
use codec::UserError::*;
use frame::{self, Reason};
use proto::*;

use bytes::Buf;

use std::io;

/// Manages state transitions related to outbound frames.
#[derive(Debug)]
pub(super) struct Send<B, P>
where
    P: Peer,
{
    /// Stream identifier to use for next initialized stream.
    next_stream_id: Result<StreamId, StreamIdOverflow>,

    /// Initial window size of locally initiated streams
    init_window_sz: WindowSize,

    /// Prioritization layer
    prioritize: Prioritize<B, P>,
}

impl<B, P> Send<B, P>
where
    B: Buf,
    P: Peer,
{
    /// Create a new `Send`
    pub fn new(config: &Config) -> Self {
        Send {
            init_window_sz: config.local_init_window_sz,
            next_stream_id: Ok(config.local_next_stream_id),
            prioritize: Prioritize::new(config),
        }
    }

    /// Returns the initial send window size
    pub fn init_window_sz(&self) -> WindowSize {
        self.init_window_sz
    }

    pub fn open(&mut self) -> Result<StreamId, UserError> {
        let stream_id = self.try_open()?;
        self.next_stream_id = stream_id.next_id();
        Ok(stream_id)
    }

    pub fn send_headers(
        &mut self,
        frame: frame::Headers,
        stream: &mut store::Ptr<B, P>,
        counts: &mut Counts<P>,
        task: &mut Option<Task>,
    ) -> Result<(), UserError> {
        trace!(
            "send_headers; frame={:?}; init_window={:?}",
            frame,
            self.init_window_sz
        );

        let end_stream = frame.is_end_stream();

        // Update the state
        stream.state.send_open(end_stream)?;

        if P::is_local_init(frame.stream_id()) {
            if counts.can_inc_num_send_streams() {
                counts.inc_num_send_streams();
            } else {
                self.prioritize.queue_open(stream);
            }
        }

        // Queue the frame for sending
        self.prioritize.queue_frame(frame.into(), stream, task);

        Ok(())
    }

    pub fn send_reset(
        &mut self,
        reason: Reason,
        stream: &mut store::Ptr<B, P>,
        task: &mut Option<Task>,
    ) {
        if stream.state.is_reset() {
            // Don't double reset
            return;
        }

        // If closed AND the send queue is flushed, then the stream cannot be
        // reset either
        if stream.state.is_closed() && stream.pending_send.is_empty() {
            return;
        }

        // Transition the state
        stream.state.set_reset(reason);

        self.recv_err(stream);

        let frame = frame::Reset::new(stream.id, reason);

        trace!("send_reset -- queueing; frame={:?}", frame);
        self.prioritize.queue_frame(frame.into(), stream, task);


    }

    pub fn send_data(
        &mut self,
        frame: frame::Data<B>,
        stream: &mut store::Ptr<B, P>,
        task: &mut Option<Task>,
    ) -> Result<(), UserError> {
        self.prioritize.send_data(frame, stream, task)
    }

    pub fn send_trailers(
        &mut self,
        frame: frame::Headers,
        stream: &mut store::Ptr<B, P>,
        task: &mut Option<Task>,
    ) -> Result<(), UserError> {
        // TODO: Should this logic be moved into state.rs?
        if !stream.state.is_send_streaming() {
            return Err(UnexpectedFrameType.into());
        }

        stream.state.send_close();

        trace!("send_trailers -- queuing; frame={:?}", frame);
        self.prioritize.queue_frame(frame.into(), stream, task);

        // Release any excess capacity
        self.prioritize.reserve_capacity(0, stream);

        Ok(())
    }

    pub fn poll_complete<T>(
        &mut self,
        store: &mut Store<B, P>,
        counts: &mut Counts<P>,
        dst: &mut Codec<T, Prioritized<B>>,
    ) -> Poll<(), io::Error>
    where
        T: AsyncWrite,
    {
        self.prioritize.poll_complete(store, counts, dst)
    }

    /// Request capacity to send data
    pub fn reserve_capacity(&mut self, capacity: WindowSize, stream: &mut store::Ptr<B, P>) {
        self.prioritize.reserve_capacity(capacity, stream)
    }

    pub fn poll_capacity(
        &mut self,
        stream: &mut store::Ptr<B, P>,
    ) -> Poll<Option<WindowSize>, UserError> {
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
    pub fn capacity(&self, stream: &mut store::Ptr<B, P>) -> WindowSize {
        let available = stream.send_flow.available();
        let buffered = stream.buffered_send_data;

        if available <= buffered {
            0
        } else {
            available - buffered
        }
    }

    pub fn recv_connection_window_update(
        &mut self,
        frame: frame::WindowUpdate,
        store: &mut Store<B, P>,
    ) -> Result<(), Reason> {
        self.prioritize
            .recv_connection_window_update(frame.size_increment(), store)
    }

    pub fn recv_stream_window_update(
        &mut self,
        sz: WindowSize,
        stream: &mut store::Ptr<B, P>,
        task: &mut Option<Task>,
    ) -> Result<(), Reason> {
        if let Err(e) = self.prioritize.recv_stream_window_update(sz, stream) {
            debug!("recv_stream_window_update !!; err={:?}", e);
            self.send_reset(FlowControlError.into(), stream, task);

            return Err(e);
        }

        Ok(())
    }

    pub fn recv_err(&mut self, stream: &mut store::Ptr<B, P>) {
        // Clear all pending outbound frames
        self.prioritize.clear_queue(stream);

        // Reclaim all capacity assigned to the stream and re-assign it to the
        // connection
        let available = stream.send_flow.available();
        stream.send_flow.claim_capacity(available);
        // Re-assign all capacity to the connection
        self.prioritize
            .assign_connection_capacity(available, stream);
    }

    pub fn apply_remote_settings(
        &mut self,
        settings: &frame::Settings,
        store: &mut Store<B, P>,
        task: &mut Option<Task>,
    ) -> Result<(), RecvError> {
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

                trace!("decrementing all windows; dec={}", dec);

                store.for_each(|mut stream| {
                    let stream = &mut *stream;

                    stream.send_flow.dec_window(dec);
                    trace!(
                        "decremented stream window; id={:?}; decr={}; flow={:?}",
                        stream.id,
                        dec,
                        stream.send_flow
                    );

                    // TODO: Probably try to assign capacity?

                    // TODO: Handle reclaiming connection level window
                    // capacity.

                    // TODO: Should this notify the producer?

                    Ok::<_, RecvError>(())
                })?;
            } else if val > old_val {
                let inc = val - old_val;

                store.for_each(|mut stream| {
                    self.recv_stream_window_update(inc, &mut stream, task)
                        .map_err(RecvError::Connection)
                })?;
            }
        }

        Ok(())
    }

    pub fn ensure_not_idle(&self, id: StreamId) -> Result<(), Reason> {
        if let Ok(next) = self.next_stream_id {
            if id >= next {
                return Err(ProtocolError);
            }
        }
        // if next_stream_id is overflowed, that's ok.

        Ok(())
    }

    pub fn ensure_next_stream_id(&self) -> Result<StreamId, UserError> {
        self.next_stream_id.map_err(|_| OverflowedStreamId)
    }

    /// Returns a new StreamId if the local actor can initiate a new stream.
    fn try_open(&self) -> Result<StreamId, UserError> {
        if P::is_server() {
            // Servers cannot open streams. PushPromise must first be reserved.
            return Err(UnexpectedFrameType);
        }

        self.ensure_next_stream_id()
    }
}
