use crate::codec::Codec;
use crate::frame::Ping;
use crate::proto::{self, PingPayload};

use bytes::Buf;
use futures_util::task::AtomicWaker;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::AsyncWrite;

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
    ping_task: AtomicWaker,
    /// Task to wake up `share::PingPong::poll_pong`.
    pong_task: AtomicWaker,
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
            ping_task: AtomicWaker::new(),
            pong_task: AtomicWaker::new(),
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
                    log::trace!("recv PING SHUTDOWN ack");
                    return ReceivedPing::Shutdown;
                }

                // if not the payload we expected, put it back.
                self.pending_ping = Some(pending);
            }

            if let Some(ref users) = self.user_pings {
                if users.receive_pong(&ping) {
                    log::trace!("recv PING USER ack");
                    return ReceivedPing::Unknown;
                }
            }

            // else we were acked a ping we didn't send?
            // The spec doesn't require us to do anything about this,
            // so for resiliency, just ignore it for now.
            log::warn!("recv PING ack that we never sent: {:?}", ping);
            ReceivedPing::Unknown
        } else {
            // Save the ping's payload to be sent as an acknowledgement.
            self.pending_pong = Some(ping.into_payload());
            ReceivedPing::MustAck
        }
    }

    /// Send any pending pongs.
    pub(crate) fn send_pending_pong<T, B>(
        &mut self,
        cx: &mut Context,
        dst: &mut Codec<T, B>,
    ) -> Poll<io::Result<()>>
    where
        T: AsyncWrite + Unpin,
        B: Buf,
    {
        if let Some(pong) = self.pending_pong.take() {
            if !dst.poll_ready(cx)?.is_ready() {
                self.pending_pong = Some(pong);
                return Poll::Pending;
            }

            dst.buffer(Ping::pong(pong).into())
                .expect("invalid pong frame");
        }

        Poll::Ready(Ok(()))
    }

    /// Send any pending pings.
    pub(crate) fn send_pending_ping<T, B>(
        &mut self,
        cx: &mut Context,
        dst: &mut Codec<T, B>,
    ) -> Poll<io::Result<()>>
    where
        T: AsyncWrite + Unpin,
        B: Buf,
    {
        if let Some(ref mut ping) = self.pending_ping {
            if !ping.sent {
                if !dst.poll_ready(cx)?.is_ready() {
                    return Poll::Pending;
                }

                dst.buffer(Ping::new(ping.payload).into())
                    .expect("invalid ping frame");
                ping.sent = true;
            }
        } else if let Some(ref users) = self.user_pings {
            users.0.ping_task.register(cx.waker());
            let mut curr = users.0.state.load(Ordering::Acquire);

            'state: loop {
                let mut state = UserState::decode(curr);

                'pings: for id in 0..MAX_USER_PINGS {
                    match state.pings[id] {
                        PingState::PingQueued => (), // ok
                        _ => continue 'pings,
                    }
                    if !dst.poll_ready(cx)?.is_ready() {
                        return Poll::Pending;
                    }
                    state.pings[id] = PingState::Inflight;

                    match users.0.state.compare_exchange(
                        curr,
                        state.encode(),
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => dst
                            .buffer(Ping::new(Ping::USERS[id]).into())
                            .expect("invalid ping frame"),
                        Err(actual) => {
                            curr = actual;
                            continue 'state;
                        }
                    }
                }

                break 'state;
            }
        }

        Poll::Ready(Ok(()))
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
    pub(crate) fn send_ping(&self, id: u8) -> Result<(), Option<proto::Error>> {
        assert!(
            MAX_USER_PINGS > id as usize,
            "internal ping id overflow: {}",
            id
        );
        let id = id as usize;

        let mut curr = self.0.state.load(Ordering::Acquire);

        loop {
            let mut state = UserState::decode(curr);

            if state.closed {
                return Err(Some(broken_pipe().into()));
            } else if let PingState::Empty = state.pings[id] {
                state.pings[id] = PingState::PingQueued;
            } else {
                // Was already pending, user error!
                return Err(None);
            }

            match self.0.state.compare_exchange(
                curr,
                state.encode(),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.0.ping_task.wake();
                    return Ok(());
                }
                Err(actual) => curr = actual,
            }
        }
    }

    pub(crate) fn poll_pong(&self, cx: &mut Context) -> Poll<Result<u8, proto::Error>> {
        // Must register before checking state, in case state were to change
        // before we could register, and then the ping would just be lost.
        self.0.pong_task.register(cx.waker());

        let mut curr = self.0.state.load(Ordering::Acquire);

        loop {
            let mut state = UserState::decode(curr);
            let id;

            if state.closed {
                return Poll::Ready(Err(broken_pipe().into()));
            } else if let PingState::Pong = state.pings[0] {
                id = 0;
                state.pings[0] = PingState::Empty;
            } else if let PingState::Pong = state.pings[1] {
                id = 1;
                state.pings[1] = PingState::Empty;
            } else {
                return Poll::Pending;
            }

            match self.0.state.compare_exchange(
                curr,
                state.encode(),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Poll::Ready(Ok(id)),
                Err(actual) => curr = actual,
            }
        }
    }
}

// ===== impl UserPingsRx =====

impl UserPingsRx {
    fn receive_pong(&self, pong: &Ping) -> bool {
        debug_assert!(pong.is_ack());

        let id = if let Some(id) = pong.user_payload_id() {
            if (id as usize) < MAX_USER_PINGS {
                id as usize
            } else {
                return false;
            }
        } else {
            return false;
        };

        let mut curr = self.0.state.load(Ordering::Acquire);

        loop {
            let mut state = UserState::decode(curr);
            if let PingState::Inflight = state.pings[id] {
                state.pings[id] = PingState::Pong;
            } else {
                return false;
            }

            match self.0.state.compare_exchange(
                curr,
                state.encode(),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.0.pong_task.wake();
                    return true;
                }
                Err(actual) => curr = actual,
            }
        }
    }
}

impl Drop for UserPingsRx {
    fn drop(&mut self) {
        self.0.state.store(USER_STATE_CLOSED, Ordering::Release);
        self.0.pong_task.wake();
    }
}

fn broken_pipe() -> io::Error {
    io::ErrorKind::BrokenPipe.into()
}

// ===== impl UserState =====

/// No user ping pending.
const USER_STATE_EMPTY: usize = 0;
/// User has called `send_ping`, but PING hasn't been written yet.
const USER_STATE_PENDING_PING: usize = 0b01;
/// User PING has been written, waiting for PONG.
const USER_STATE_PENDING_PONG: usize = 0b11;
/// We've received user PONG, waiting for user to `poll_pong`.
const USER_STATE_RECEIVED_PONG: usize = 0b10;

const USER_STATE_MASK: usize = 0b11;

/// The connection is closed.
const USER_STATE_CLOSED: usize = usize::max_value() - (usize::max_value() >> 1);

const MAX_USER_PINGS: usize = 2;

struct UserState {
    pings: [PingState; MAX_USER_PINGS],
    closed: bool,
}

enum PingState {
    Empty,
    PingQueued,
    Inflight,
    Pong,
}

impl UserState {
    fn decode(bits: usize) -> Self {
        let closed = bits & USER_STATE_CLOSED == USER_STATE_CLOSED;

        let pings = [PingState::decode(bits, 0), PingState::decode(bits, 1)];

        UserState { pings, closed }
    }

    fn encode(&self) -> usize {
        let mut bits = if self.closed { USER_STATE_CLOSED } else { 0 };

        for i in 0..MAX_USER_PINGS {
            bits |= self.pings[i].encode(i as u8);
        }

        bits
    }
}

impl PingState {
    fn decode(bits: usize, id: u8) -> Self {
        let id = id * 2;
        let mask = USER_STATE_MASK << id;
        match (bits & mask) >> id {
            USER_STATE_EMPTY => PingState::Empty,
            USER_STATE_PENDING_PING => PingState::PingQueued,
            USER_STATE_PENDING_PONG => PingState::Inflight,
            USER_STATE_RECEIVED_PONG => PingState::Pong,
            _ => unreachable!(),
        }
    }

    fn encode(&self, id: u8) -> usize {
        let id = id * 2;
        (match *self {
            PingState::Empty => USER_STATE_EMPTY,
            PingState::PingQueued => USER_STATE_PENDING_PING,
            PingState::Inflight => USER_STATE_PENDING_PONG,
            PingState::Pong => USER_STATE_RECEIVED_PONG,
        }) << id
    }
}
