use codec::Codec;
use frame::Ping;
use proto::{self, PingPayload};

use bytes::Buf;
use futures::{Async, Poll};
use futures::task::AtomicTask;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio_io::AsyncWrite;

/// Acknowledges ping requests from the remote.
#[derive(Debug)]
pub(crate) struct PingPong {
    pending_ping: Option<PendingPing>,
    pending_pong: Option<PingPayload>,
    user_pings: Option<UserPingsRx>,
}

#[derive(Debug)]
pub(crate) struct UserPings(Arc<UserPingsInner>);

#[derive(Debug)]
struct UserPingsRx(Arc<UserPingsInner>);

#[derive(Debug)]
struct UserPingsInner {
    state: AtomicUsize,
    /// Task to wake up the main `Connection`.
    ping_task: AtomicTask,
    /// Task to wake up `share::PingPong::poll_pong`.
    pong_task: AtomicTask,
}

#[derive(Debug)]
struct PendingPing {
    payload: PingPayload,
    sent: bool,
}

/// Status returned from `PingPong::recv_ping`.
#[derive(Debug)]
pub(crate) enum ReceivedPing {
    MustAck,
    Unknown,
    Shutdown,
}

/// No user ping pending.
const USER_STATE_EMPTY: usize = 0;
/// User has called `send_ping`, but PING hasn't been written yet.
const USER_STATE_PENDING_PING: usize = 1;
/// User PING has been written, waiting for PONG.
const USER_STATE_PENDING_PONG: usize = 2;
/// We've received user PONG, waiting for user to `poll_pong`.
const USER_STATE_RECEIVED_PONG: usize = 3;
/// The connection is closed.
const USER_STATE_CLOSED: usize = 4;

// ===== impl PingPong =====

impl PingPong {
    pub(crate) fn new() -> Self {
        PingPong {
            pending_ping: None,
            pending_pong: None,
            user_pings: None,
        }
    }

    /// Can only be called once. If called a second time, returns `None`.
    pub(crate) fn take_user_pings(&mut self) -> Option<UserPings> {
        if self.user_pings.is_some() {
            return None;
        }

        let user_pings = Arc::new(UserPingsInner {
            state: AtomicUsize::new(USER_STATE_EMPTY),
            ping_task: AtomicTask::new(),
            pong_task: AtomicTask::new(),
        });
        self.user_pings = Some(UserPingsRx(user_pings.clone()));
        Some(UserPings(user_pings))
    }

    pub(crate) fn ping_shutdown(&mut self) {
        assert!(self.pending_ping.is_none());

        self.pending_ping = Some(PendingPing {
            payload: Ping::SHUTDOWN,
            sent: false,
        });
    }

    /// Process a ping
    pub(crate) fn recv_ping(&mut self, ping: Ping) -> ReceivedPing {
        // The caller should always check that `send_pongs` returns ready before
        // calling `recv_ping`.
        assert!(self.pending_pong.is_none());

        if ping.is_ack() {
            if let Some(pending) = self.pending_ping.take() {
                if &pending.payload == ping.payload() {
                    assert_eq!(
                        &pending.payload,
                        &Ping::SHUTDOWN,
                        "pending_ping should be for shutdown",
                    );
                    trace!("recv PING SHUTDOWN ack");
                    return ReceivedPing::Shutdown;
                }

                // if not the payload we expected, put it back.
                self.pending_ping = Some(pending);
            }

            if let Some(ref users) = self.user_pings {
                if ping.payload() == &Ping::USER && users.receive_pong() {
                    trace!("recv PING USER ack");
                    return ReceivedPing::Unknown;
                }
            }

            // else we were acked a ping we didn't send?
            // The spec doesn't require us to do anything about this,
            // so for resiliency, just ignore it for now.
            warn!("recv PING ack that we never sent: {:?}", ping);
            ReceivedPing::Unknown
        } else {
            // Save the ping's payload to be sent as an acknowledgement.
            self.pending_pong = Some(ping.into_payload());
            ReceivedPing::MustAck
        }
    }

    /// Send any pending pongs.
    pub(crate) fn send_pending_pong<T, B>(&mut self, dst: &mut Codec<T, B>) -> Poll<(), io::Error>
    where
        T: AsyncWrite,
        B: Buf,
    {
        if let Some(pong) = self.pending_pong.take() {
            if !dst.poll_ready()?.is_ready() {
                self.pending_pong = Some(pong);
                return Ok(Async::NotReady);
            }

            dst.buffer(Ping::pong(pong).into())
                .expect("invalid pong frame");
        }

        Ok(Async::Ready(()))
    }

    /// Send any pending pings.
    pub(crate) fn send_pending_ping<T, B>(&mut self, dst: &mut Codec<T, B>) -> Poll<(), io::Error>
    where
        T: AsyncWrite,
        B: Buf,
    {
        if let Some(ref mut ping) = self.pending_ping {
            if !ping.sent {
                if !dst.poll_ready()?.is_ready() {
                    return Ok(Async::NotReady);
                }

                dst.buffer(Ping::new(ping.payload).into())
                    .expect("invalid ping frame");
                ping.sent = true;
            }
        } else if let Some(ref users) = self.user_pings {
            if users.0.state.load(Ordering::Acquire) == USER_STATE_PENDING_PING {
                if !dst.poll_ready()?.is_ready() {
                    return Ok(Async::NotReady);
                }

                dst.buffer(Ping::new(Ping::USER).into())
                    .expect("invalid ping frame");
                users.0.state.store(USER_STATE_PENDING_PONG, Ordering::Release);
            } else {
                users.0.ping_task.register();
            }
        }

        Ok(Async::Ready(()))
    }
}

impl ReceivedPing {
    pub(crate) fn is_shutdown(&self) -> bool {
        match *self {
            ReceivedPing::Shutdown => true,
            _ => false,
        }
    }
}

// ===== impl UserPings =====

impl UserPings {
    pub(crate) fn send_ping(&self) -> Result<(), Option<proto::Error>> {
        let prev = self.0.state.compare_and_swap(
            USER_STATE_EMPTY, // current
            USER_STATE_PENDING_PING, // new
            Ordering::AcqRel,
        );

        match prev {
            USER_STATE_EMPTY => {
                self.0.ping_task.notify();
                Ok(())
            },
            USER_STATE_CLOSED => {
                Err(Some(broken_pipe().into()))
            }
            _ => {
                // Was already pending, user error!
                Err(None)
            }
        }
    }

    pub(crate) fn poll_pong(&self) -> Poll<(), proto::Error> {
        // Must register before checking state, in case state were to change
        // before we could register, and then the ping would just be lost.
        self.0.pong_task.register();
        let prev = self.0.state.compare_and_swap(
            USER_STATE_RECEIVED_PONG, // current
            USER_STATE_EMPTY, // new
            Ordering::AcqRel,
        );

        match prev {
            USER_STATE_RECEIVED_PONG => Ok(Async::Ready(())),
            USER_STATE_CLOSED => Err(broken_pipe().into()),
            _ => Ok(Async::NotReady),
        }
    }
}

// ===== impl UserPingsRx =====

impl UserPingsRx {
    fn receive_pong(&self) -> bool {
        let prev = self.0.state.compare_and_swap(
            USER_STATE_PENDING_PONG, // current
            USER_STATE_RECEIVED_PONG, // new
            Ordering::AcqRel,
        );

        if prev == USER_STATE_PENDING_PONG {
            self.0.pong_task.notify();
            true
        } else {
            false
        }
    }
}

impl Drop for UserPingsRx {
    fn drop(&mut self) {
        self.0.state.store(USER_STATE_CLOSED, Ordering::Release);
        self.0.pong_task.notify();
    }
}

fn broken_pipe() -> io::Error {
    io::ErrorKind::BrokenPipe.into()
}
