use ConnectionError;
use frame::{self, Frame};
use proto::*;

/// Tracks END_STREAM frames sent from the local peer.
#[derive(Debug)]
pub struct StreamSendClose<T> {
    inner: T,
}

impl<T, U> StreamSendClose<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams,
{
    pub fn new(inner: T) -> StreamSendClose<T> {
        StreamSendClose { inner }
    }
}

/// Tracks END_STREAM frames sent from the local peer.
impl<T, U> Sink for StreamSendClose<T>
    where T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams,
{
    type SinkItem = Frame<U>;
    type SinkError = ConnectionError;

    fn start_send(&mut self, frame: Self::SinkItem) -> StartSend<Frame<U>, ConnectionError> {
        let id = frame.stream_id();
        let eos = frame.is_end_stream();
        trace!("start_send: id={:?} eos={}", id, eos);
        if !id.is_zero() {
            if eos {
                if let &Frame::Reset(ref rst) = &frame {
                    self.streams_mut().reset_stream(id, rst.reason());
                } else {
                    debug_assert!(self.streams().is_active(id));
                    self.streams_mut().close_send_half(id)?;
                }
            }
        }

        self.inner.start_send(frame)
    }

    fn poll_complete(&mut self) -> Poll<(), ConnectionError> {
        self.inner.poll_complete()
    }
}

proxy_apply_settings!(StreamSendClose);
proxy_control_flow!(StreamSendClose);
proxy_control_streams!(StreamSendClose);
proxy_control_ping!(StreamSendClose);
proxy_stream!(StreamSendClose);
proxy_ready_sink!(StreamSendClose; ControlStreams);
