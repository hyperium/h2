use {frame, ConnectionError};
use proto::*;

#[derive(Debug)]
pub struct Settings {
    pending_ack: bool,
}

impl Settings {
    pub fn new() -> Self {
        Settings {
            pending_ack: false,
        }
    }

    pub fn recv_settings(&mut self, frame: frame::Settings) {
        if frame.is_ack() {
            debug!("received remote settings ack");
            // TODO: handle acks
        } else {
            assert!(!self.pending_ack);
            self.pending_ack = true;
        }
    }

    pub fn send_pending_ack<T, B>(&mut self, dst: &mut Codec<T, B>)
        -> Poll<(), ConnectionError>
        where T: AsyncWrite,
              B: Buf,
    {
        if self.pending_ack {
            let frame = frame::Settings::ack();

            match dst.start_send(frame.into())? {
                AsyncSink::Ready => {
                    self.pending_ack = false;
                    return Ok(().into());
                }
                AsyncSink::NotReady(_) => {
                    return Ok(Async::NotReady);
                }
            }
        }

        Ok(().into())
    }
}
