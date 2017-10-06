use codec::Codec;
use frame::Ping;
use proto::PingPayload;

use bytes::Buf;
use futures::{Async, Poll};
use std::io;
use tokio_io::AsyncWrite;

/// Acknowledges ping requests from the remote.
#[derive(Debug)]
pub struct PingPong {
    sending_pong: Option<PingPayload>,
}

impl PingPong {
    pub fn new() -> Self {
        PingPong {
            sending_pong: None,
        }
    }

    /// Process a ping
    pub fn recv_ping(&mut self, ping: Ping) {
        // The caller should always check that `send_pongs` returns ready before
        // calling `recv_ping`.
        assert!(self.sending_pong.is_none());

        if !ping.is_ack() {
            // Save the ping's payload to be sent as an acknowledgement.
            self.sending_pong = Some(ping.into_payload());
        }
    }

    /// Send any pending pongs.
    pub fn send_pending_pong<T, B>(&mut self, dst: &mut Codec<T, B>) -> Poll<(), io::Error>
    where
        T: AsyncWrite,
        B: Buf,
    {
        if let Some(pong) = self.sending_pong.take() {
            if !dst.poll_ready()?.is_ready() {
                self.sending_pong = Some(pong);
                return Ok(Async::NotReady);
            }

            dst.buffer(Ping::pong(pong).into()).ok().expect("invalid pong frame");
        }

        Ok(Async::Ready(()))
    }
}
