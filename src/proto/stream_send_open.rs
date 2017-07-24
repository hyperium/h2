use ConnectionError;
use error::User::{InactiveStreamId, InvalidStreamId, StreamReset, Rejected, UnexpectedFrameType};
use frame::{Frame, SettingSet};
use proto::*;

/// Ensures that frames are sent on open streams in the appropriate state.
#[derive(Debug)]
pub struct StreamSendOpen<T> {
    inner: T,

    max_concurrency: Option<u32>,
    initial_window_size: WindowSize,
}

impl<T, U> StreamSendOpen<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams,
{
    pub fn new(initial_window_size: WindowSize,
               max_concurrency: Option<u32>,
               inner: T)
            -> StreamSendOpen<T>
    {
        StreamSendOpen {
            inner,
            max_concurrency,
            initial_window_size,
        }
    }
}

/// Handles updates to `SETTINGS_MAX_CONCURRENT_STREAMS` from the remote peer.
impl<T: ApplySettings> ApplySettings for StreamSendOpen<T> {
    fn apply_local_settings(&mut self, set: &SettingSet) -> Result<(), ConnectionError> {
        self.inner.apply_local_settings(set)
    }

    fn apply_remote_settings(&mut self, set: &SettingSet) -> Result<(), ConnectionError> {
        self.max_concurrency = set.max_concurrent_streams();
        if let Some(sz) = set.initial_window_size() {
            self.initial_window_size = sz;
        }
        self.inner.apply_remote_settings(set)
    }
}

/// Helper.
impl<T: ControlStreams> StreamSendOpen<T> {
    fn check_not_reset(&self, id: StreamId) -> Result<(), ConnectionError> {
        // Ensure that the stream hasn't been closed otherwise.
        match self.inner.get_reset(id) {
            Some(reason) => Err(StreamReset(reason).into()),
            None => Ok(()),
        }
    }
}

/// Ensures that frames are sent on open streams in the appropriate state.
impl<T, U> Sink for StreamSendOpen<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams,
{
    type SinkItem = T::SinkItem;
    type SinkError = T::SinkError;

    fn start_send(&mut self, frame: T::SinkItem) -> StartSend<T::SinkItem, T::SinkError> {
        let id = frame.stream_id();
        trace!("start_send: id={:?}", id);

        // Forward connection frames immediately.
        if id.is_zero() {
            if !frame.is_connection_frame() {
                return Err(InvalidStreamId.into());
            }

            return self.inner.start_send(frame);
        }

        match &frame {
            &Frame::Reset(..) => {}

            &Frame::Headers(..) => {
                self.check_not_reset(id)?;
                if T::local_valid_id(id) {
                    if self.inner.is_local_active(id) {
                        // Can't send a a HEADERS frame on a local stream that's active,
                        // because we've already sent headers.  This will have to change
                        // to support PUSH_PROMISE.
                        return Err(UnexpectedFrameType.into());
                    }

                    if !T::local_can_open() {
                        // A server tried to start a stream with a HEADERS frame.
                        return Err(UnexpectedFrameType.into());
                    }

                    if let Some(max) = self.max_concurrency {
                        // Don't allow this stream to overflow the remote's max stream
                        // concurrency.
                        if (max as usize) < self.inner.local_active_len() {
                            return Err(Rejected.into());
                        }
                    }

                    self.inner.local_open(id, self.initial_window_size)?;
                } else {
                    // On remote streams,
                    if self.inner.remote_open_send_half(id, self.initial_window_size).is_err() {
                        return Err(InvalidStreamId.into());
                    }
                }
            }

            // This only handles other stream frames (data, window update, ...).  Ensure
            // the stream is open (i.e. has already sent headers).
            _ => {
                self.check_not_reset(id)?;
                if !self.inner.is_send_open(id) {
                    return Err(InactiveStreamId.into());
                }
            }
        }

        self.inner.start_send(frame)
    }

    fn poll_complete(&mut self) -> Poll<(), T::SinkError> {
        self.inner.poll_complete()
    }
}

proxy_control_flow!(StreamSendOpen);
proxy_control_streams!(StreamSendOpen);
proxy_control_ping!(StreamSendOpen);
proxy_stream!(StreamSendOpen);
proxy_ready_sink!(StreamSendOpen; ControlStreams);
