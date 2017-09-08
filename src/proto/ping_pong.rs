use frame::Ping;
use proto::*;

use std::io;

/// Acknowledges ping requests from the remote.
#[derive(Debug)]
pub struct PingPong<B> {
    // TODO: this doesn't need to save the entire frame
    sending_pong: Option<Frame<B>>,
    received_pong: Option<PingPayload>,
    // TODO: factor this out
    blocked_ping: Option<task::Task>,
}

impl<B> PingPong<B>
where
    B: Buf,
{
    pub fn new() -> Self {
        PingPong {
            sending_pong: None,
            received_pong: None,
            blocked_ping: None,
        }
    }

    /// Process a ping
    pub fn recv_ping(&mut self, ping: Ping) {
        // The caller should always check that `send_pongs` returns ready before
        // calling `recv_ping`.
        assert!(self.sending_pong.is_none());

        if ping.is_ack() {
            // Save acknowledgements to be returned from take_pong().
            self.received_pong = Some(ping.into_payload());

            if let Some(task) = self.blocked_ping.take() {
                task.notify();
            }
        } else {
            // Save the ping's payload to be sent as an acknowledgement.
            let pong = Ping::pong(ping.into_payload());
            self.sending_pong = Some(pong.into());
        }
    }

    /// Send any pending pongs.
    pub fn send_pending_pong<T>(&mut self, dst: &mut Codec<T, B>) -> Poll<(), io::Error>
    where
        T: AsyncWrite,
    {
        if let Some(pong) = self.sending_pong.take() {
            if !dst.poll_ready()?.is_ready() {
                self.sending_pong = Some(pong);
                return Ok(Async::NotReady);
            }

            dst.buffer(pong).ok().expect("invalid pong frame");
        }

        Ok(Async::Ready(()))
    }
}
