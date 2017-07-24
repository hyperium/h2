use ConnectionError;
use error::Reason;
use frame::{self, Frame};
use proto::*;
use proto::ready::ReadySink;

/// Tracks END_STREAM frames received from the remote peer.
#[derive(Debug)]
pub struct StreamRecvClose<T> {
    inner: T,
}

impl<T, U> StreamRecvClose<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: Sink<SinkItem = Frame<U>, SinkError = ConnectionError>,
          T: ControlStreams,
{
    pub fn new(inner: T) -> StreamRecvClose<T> {
        StreamRecvClose { inner }
    }
}

/// Tracks END_STREAM frames received from the remote peer.
impl<T> Stream for StreamRecvClose<T>
    where T: Stream<Item = Frame, Error = ConnectionError>,
          T: ControlStreams,
{
    type Item = T::Item;
    type Error = T::Error;

    fn poll(&mut self) -> Poll<Option<T::Item>, T::Error> {
        let frame = match try_ready!(self.inner.poll()) {
            None => return Ok(Async::Ready(None)),
            Some(f) => f,
        };

        let id = frame.stream_id();
        if !id.is_zero() {
            if frame.is_end_stream() {
                trace!("poll: id={:?} eos", id);
                if let &Frame::Reset(ref rst) = &frame {
                    self.inner.reset_stream(id, rst.reason());
                } else {
                    debug_assert!(self.inner.is_active(id));
                    self.inner.close_recv_half(id)?;
                }
            }
        }

        Ok(Async::Ready(Some(frame)))
    }
}

proxy_apply_settings!(StreamRecvClose);
proxy_control_flow!(StreamRecvClose);
proxy_control_streams!(StreamRecvClose);
proxy_control_ping!(StreamRecvClose);
proxy_sink!(StreamRecvClose);
proxy_ready_sink!(StreamRecvClose);
