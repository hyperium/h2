//! Bounded, independently cancellable user PING exchanges.
use crate::codec::{Codec, UserError};
use crate::frame::Ping;
use crate::proto::{Error, PingPayload};
use atomic_waker::AtomicWaker;
use bytes::Buf;
use std::collections::BTreeMap;
use std::io;
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};
use tokio::io::AsyncWrite;

const LIMIT: usize = 64;

#[derive(Debug)]
pub(crate) struct ConcurrentPings {
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    closed: bool,
    next: Option<u64>,
    entries: BTreeMap<u64, Entry>,
}

#[derive(Debug)]
struct Entry {
    sent: bool,
    received: bool,
    abandoned: bool,
    waker: Option<Waker>,
}

#[derive(Debug)]
pub(crate) struct ConcurrentPing<'a> {
    shared: &'a ConcurrentPings,
    legacy_task: &'a AtomicWaker,
    id: u64,
}

impl ConcurrentPings {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(State {
                closed: false,
                next: Some(0),
                entries: BTreeMap::new(),
            }),
        }
    }

    pub(crate) fn send<'a>(
        &'a self,
        check_legacy: impl FnOnce() -> Result<(), crate::Error>,
        legacy_task: &'a AtomicWaker,
    ) -> Result<ConcurrentPing<'a>, crate::Error> {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return Err(closed().into());
        }
        check_legacy()?;
        if state.entries.len() == LIMIT || state.entries.values().any(|entry| entry.abandoned) {
            return Err(UserError::SendPingWhilePending.into());
        }
        // Never reuse IDs, including after cancellation. Skip protocol-internal
        // and legacy user payloads, so late ACKs cannot alias either API.
        let id = loop {
            let id = state.next.ok_or(UserError::Rejected)?;
            state.next = id.checked_add(1);
            let payload = id.to_be_bytes();
            if payload != Ping::USER && payload != Ping::SHUTDOWN {
                break id;
            }
        };
        state.entries.insert(
            id,
            Entry {
                sent: false,
                received: false,
                abandoned: false,
                waker: None,
            },
        );
        drop(state);
        Ok(ConcurrentPing {
            shared: self,
            legacy_task,
            id,
        })
    }

    // Serialize the legacy atomic transition with high-level first-poll gates.
    // The callback only changes the legacy atomic state; wakeups happen later.
    pub(crate) fn with_legacy_send(&self, send: impl FnOnce() -> usize) -> Option<usize> {
        let state = self.state.lock().unwrap();
        if !state.entries.is_empty() {
            return None;
        }
        Some(send())
    }

    pub(crate) fn poll_abandoned(&self) -> Option<Poll<()>> {
        let mut state = self.state.lock().unwrap();
        let (&id, entry) = state.entries.iter().find(|(_, entry)| entry.abandoned)?;
        if !entry.received {
            return Some(Poll::Pending);
        }
        state.entries.remove(&id);
        Some(Poll::Ready(()))
    }

    pub(crate) fn send_pending<T, B>(
        &self,
        cx: &mut Context<'_>,
        dst: &mut Codec<T, B>,
    ) -> Poll<io::Result<()>>
    where
        T: AsyncWrite + Unpin,
        B: Buf,
    {
        // Never hold the state lock while polling user-supplied I/O: a
        // transport may reenter cancellation. Recheck after readiness because
        // a queued response can be dropped while the transport is polled.
        for _ in 0..LIMIT {
            if !self.state.lock().unwrap().entries.values().any(|e| !e.sent) {
                break;
            }
            if dst.poll_ready(cx)?.is_pending() {
                return Poll::Pending;
            }
            let mut state = self.state.lock().unwrap();
            if let Some((&id, entry)) = state.entries.iter_mut().find(|(_, e)| !e.sent) {
                dst.buffer(Ping::new(id.to_be_bytes()).into())
                    .expect("invalid ping frame");
                entry.sent = true;
            }
        }
        // A concurrent enqueue can refill slots during this bounded batch.
        let has_pending = self.state.lock().unwrap().entries.values().any(|e| !e.sent);
        if has_pending {
            cx.waker().wake_by_ref();
        }
        Poll::Ready(Ok(()))
    }

    pub(crate) fn receive(&self, payload: &PingPayload) -> bool {
        let mut state = self.state.lock().unwrap();
        let entry = match state.entries.get_mut(&u64::from_be_bytes(*payload)) {
            Some(entry) if entry.sent && !entry.received => entry,
            _ => return false,
        };
        entry.received = true;
        let waker = entry.waker.take();
        drop(state);
        if let Some(waker) = waker {
            waker.wake();
        }
        true
    }

    pub(crate) fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        // Move entries out before waking or dropping user wakers.
        let entries = std::mem::take(&mut state.entries);
        drop(state);
        for entry in entries.into_values() {
            if let Some(waker) = entry.waker {
                waker.wake();
            }
        }
    }
}

impl ConcurrentPing<'_> {
    pub(crate) fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        let waker = cx.waker().clone();
        let mut state = self.shared.state.lock().unwrap();
        if let Some(entry) = state.entries.get_mut(&self.id) {
            if entry.received {
                let removed = state.entries.remove(&self.id);
                drop(state);
                drop(removed);
                return Poll::Ready(Ok(()));
            }
            let previous = entry.waker.replace(waker);
            drop(state);
            drop(previous);
            Poll::Pending
        } else {
            Poll::Ready(Err(closed()))
        }
    }
}

impl Drop for ConcurrentPing<'_> {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap();
        let previous = if let Some(entry) = state.entries.get_mut(&self.id) {
            // Original ping() cancellation leaves its exchange pending for
            // poll_pong to drain. Extend that behavior to a bounded FIFO of
            // abandoned IDs, never transferring ownership of a live future.
            entry.abandoned = true;
            entry.waker.take()
        } else {
            None
        };
        drop(state);
        drop(previous);
        self.legacy_task.wake();
    }
}

fn closed() -> Error {
    io::Error::from(io::ErrorKind::BrokenPipe).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::task::Wake;

    #[derive(Default)]
    struct Counter(AtomicUsize);
    impl Wake for Counter {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn poll(ping: &mut ConcurrentPing, counter: &Arc<Counter>) -> Poll<Result<(), Error>> {
        let waker = Waker::from(counter.clone());
        ping.poll(&mut Context::from_waker(&waker))
    }

    #[test]
    fn skips_reserved_payloads_and_never_wraps() {
        let shared = ConcurrentPings::new();
        let legacy_task = AtomicWaker::new();
        for reserved in [Ping::USER, Ping::SHUTDOWN] {
            let id = u64::from_be_bytes(reserved);
            shared.state.lock().unwrap().next = Some(id);
            let ping = shared.send(|| Ok(()), &legacy_task).unwrap();
            assert_eq!(ping.id, id + 1);
            shared.state.lock().unwrap().entries.remove(&ping.id);
        }
        shared.state.lock().unwrap().next = Some(u64::MAX);
        let last = shared.send(|| Ok(()), &legacy_task).unwrap();
        assert_eq!(last.id, u64::MAX);
        assert!(shared.send(|| Ok(()), &legacy_task).is_err());
        drop(last);
    }

    #[test]
    fn ack_before_admission_is_ignored() {
        let shared = ConcurrentPings::new();
        let legacy_task = AtomicWaker::new();
        let mut ping = shared.send(|| Ok(()), &legacy_task).unwrap();
        assert!(!shared.receive(&ping.id.to_be_bytes()));
        assert!(poll(&mut ping, &Arc::default()).is_pending());
        drop(ping);
        assert_eq!(shared.state.lock().unwrap().entries.len(), 1);
        assert!(shared.send(|| Ok(()), &legacy_task).is_err());
        shared.close();
        assert!(shared.state.lock().unwrap().entries.is_empty());
    }

    #[test]
    fn independent_wakers_and_connection_drop_errors() {
        let shared = ConcurrentPings::new();
        let legacy_task = AtomicWaker::new();
        let mut a = shared.send(|| Ok(()), &legacy_task).unwrap();
        let mut b = shared.send(|| Ok(()), &legacy_task).unwrap();
        let wa = Arc::new(Counter::default());
        let wb = Arc::new(Counter::default());
        assert!(poll(&mut a, &wa).is_pending());
        assert!(poll(&mut b, &wb).is_pending());
        shared
            .state
            .lock()
            .unwrap()
            .entries
            .get_mut(&b.id)
            .unwrap()
            .sent = true;
        assert!(shared.receive(&b.id.to_be_bytes()));
        assert!(!shared.receive(&b.id.to_be_bytes()));
        assert_eq!(wa.0.load(Ordering::SeqCst), 0);
        assert_eq!(wb.0.load(Ordering::SeqCst), 1);
        shared.close();
        assert_eq!(wa.0.load(Ordering::SeqCst), 1);
        assert!(matches!(poll(&mut a, &wa), Poll::Ready(Err(_))));
        assert!(matches!(poll(&mut b, &wb), Poll::Ready(Err(_))));
        assert!(shared.state.lock().unwrap().entries.is_empty());
        assert!(shared.send(|| Ok(()), &legacy_task).is_err());
    }

    #[test]
    fn backpressure_allows_reentrant_abandonment_and_resumes() {
        use std::pin::Pin;
        use tokio::io::{AsyncRead, ReadBuf};
        struct Transport<'a> {
            shared: &'a ConcurrentPings,
            cancel: Option<ConcurrentPing<'a>>,
            blocked: bool,
            writes: usize,
        }
        impl AsyncRead for Transport<'_> {
            fn poll_read(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
                _: &mut ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                Poll::Pending
            }
        }
        impl AsyncWrite for Transport<'_> {
            fn poll_write(
                mut self: Pin<&mut Self>,
                _: &mut Context<'_>,
                bytes: &[u8],
            ) -> Poll<io::Result<usize>> {
                // A user transport can cancel a response while it is polled.
                assert!(
                    self.shared.state.try_lock().is_ok(),
                    "state locked during user I/O"
                );
                drop(self.cancel.take());
                self.writes += 1;
                if self.blocked {
                    Poll::Pending
                } else {
                    Poll::Ready(Ok(bytes.len()))
                }
            }
            fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
                Poll::Ready(Ok(()))
            }
            fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
                Poll::Ready(Ok(()))
            }
        }
        let shared = ConcurrentPings::new();
        let legacy_task = AtomicWaker::new();
        let cancel = shared.send(|| Ok(()), &legacy_task).unwrap();
        let cancel_id = cancel.id;
        let survivor = shared.send(|| Ok(()), &legacy_task).unwrap();
        let mut codec: Codec<_, bytes::Bytes> = Codec::new(Transport {
            shared: &shared,
            cancel: Some(cancel),
            blocked: true,
            writes: 0,
        });
        let waker = Waker::from(Arc::new(Counter::default()));
        let mut cx = Context::from_waker(&waker);
        // Saturate the codec with unrelated frames so send_pending must poll I/O.
        while codec.has_send_capacity() {
            codec.buffer(Ping::pong([0xff; 8]).into()).unwrap();
        }
        assert!(shared.send_pending(&mut cx, &mut codec).is_pending());
        assert!(codec.get_mut().writes > 0);
        assert_eq!(shared.state.lock().unwrap().entries.len(), 2);
        assert!(shared.state.lock().unwrap().entries[&cancel_id].abandoned);
        assert!(!shared.state.lock().unwrap().entries[&survivor.id].sent);
        codec.get_mut().blocked = false;
        assert!(matches!(
            shared.send_pending(&mut cx, &mut codec),
            Poll::Ready(Ok(()))
        ));
        assert!(shared.state.lock().unwrap().entries[&survivor.id].sent);
        assert!(shared.receive(&cancel_id.to_be_bytes()));
        assert!(matches!(shared.poll_abandoned(), Some(Poll::Ready(()))));
        assert_eq!(shared.state.lock().unwrap().entries.len(), 1);
    }
    #[test]
    fn connection_drop_invalidates_unconsumed_ack() {
        let shared = ConcurrentPings::new();
        let legacy_task = AtomicWaker::new();
        let mut ping = shared.send(|| Ok(()), &legacy_task).unwrap();
        shared
            .state
            .lock()
            .unwrap()
            .entries
            .get_mut(&ping.id)
            .unwrap()
            .sent = true;
        assert!(shared.receive(&ping.id.to_be_bytes()));
        shared.close();
        assert!(matches!(
            poll(&mut ping, &Arc::default()),
            Poll::Ready(Err(_))
        ));
        assert!(shared.state.lock().unwrap().entries.is_empty());
    }
}
