use frame;
use proto::*;

use futures::Sink;

#[derive(Debug)]
pub(crate) struct Settings {
    /// Received SETTINGS frame pending processing. The ACK must be written to
    /// the socket first then the settings applied **before** receiving any
    /// further frames.
    pending: Option<frame::Settings>,
}

impl Settings {
    pub fn new() -> Self {
        Settings {
            pending: None,
        }
    }

    pub fn recv_settings(&mut self, frame: frame::Settings) {
        if frame.is_ack() {
            debug!("received remote settings ack");
            // TODO: handle acks
        } else {
            assert!(self.pending.is_none());
            self.pending = Some(frame);
        }
    }

    pub fn send_pending_ack<T, B, C>(&mut self,
                                     dst: &mut Codec<T, B>,
                                     streams: &mut Streams<C>)
        -> Poll<(), ConnectionError>
        where T: AsyncWrite,
              B: Buf,
              C: Buf,
    {
        trace!("send_pending_ack; pending={:?}", self.pending);

        if let Some(ref settings) = self.pending {
            let frame = frame::Settings::ack();

            if let AsyncSink::NotReady(_) = dst.start_send(frame.into())? {
                trace!("failed to send ACK");
                return Ok(Async::NotReady);
            }

            trace!("ACK sent; applying settings");

            dst.apply_remote_settings(settings);
            streams.apply_remote_settings(settings)?;
        }

        self.pending = None;

        Ok(().into())
    }
}
