use crate::proto;
use crate::Reason;

use super::*;

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::sync::atomic::{AtomicU32, Ordering};
use std::task::{Context, Waker};
use std::time::Instant;

/// Tracks Stream related state
///
/// # Reference counting
///
/// There can be a number of outstanding handles to a single Stream. These are
/// tracked using reference counting. The `ref_count` field represents the
/// number of outstanding userspace handles that can reach this stream.
///
/// It's important to note that when the stream is placed in an internal queue
/// (such as an accept queue), this is **not** tracked by a reference count.
/// Thus, `ref_count` can be zero and the stream still has to be kept around.
pub(super) struct Stream {
    /// The h2 stream identifier
    pub id: StreamId,
    shared_state: Arc<Shared>,
    inner: Inner,
}

#[derive(Debug)]
pub(super) struct Shared {
    /// The h2 stream identifier
    pub id: StreamId,
    /// The store SlabIndex, if stored. If not, it will be u32::MAX.
    store: AtomicU32,
    state: Mutex<SharedState>,
    send: Mutex<Send>,
    recv: Mutex<Recv>,
}

#[derive(Debug)]
pub(super) struct SharedState {
    /// Current state of the stream
    pub state: State,

    /// Number of outstanding handles pointing to this stream
    pub ref_count: usize,

    /// Number of queued send operations waiting to be applied.
    pub pending_operation_count: usize,

    /// Set to true when a push is pending for this stream
    pub is_pending_push: bool,
}

#[derive(Debug)]
pub(super) struct Send {
    /// Send data flow control
    pub send_flow: FlowControl,

    /// Amount of send capacity that has been requested, but not yet allocated.
    pub requested_send_capacity: WindowSize,

    /// Amount of data buffered at the prioritization layer.
    /// TODO: Technically this could be greater than the window size...
    pub buffered_send_data: usize,

    /// Task tracking additional send capacity (i.e. window updates).
    send_task: Option<Waker>,

    /// Frames pending for this stream being sent to the socket
    pub pending_send: buffer::Deque,

    /// DATA frames queued by send ops but not yet applied on the connection task.
    pub pending_op_data: buffer::Deque,

    /// Set to true when the send capacity has been incremented
    pub send_capacity_inc: bool,

    /// Set to true when the stream is pending to be opened.
    pub is_pending_open: bool,
}

#[derive(Debug)]
pub(super) struct Recv {
    /// Receive data flow control
    pub recv_flow: FlowControl,

    pub in_flight_recv_data: WindowSize,

    /// Frames pending for this stream to read
    pub pending_recv: buffer::Deque,

    /// When the RecvStream drop occurs, no data should be received.
    pub is_recv: bool,

    /// Task tracking receiving frames
    pub recv_task: Option<Waker>,

    /// Task tracking pushed promises.
    pub push_task: Option<Waker>,

    /// Streams promised off this stream that are waiting to be observed.
    pub pending_push_promises: buffer::Deque,
}

#[derive(Debug)]
pub(super) struct Inner {
    /// Set to `true` when the stream is counted against the connection's max
    /// concurrent streams.
    pub is_counted: bool,

    // ===== Fields related to sending =====
    /// Next node in the accept linked list
    pub next_pending_send: Option<store::Key>,

    /// Set to true when the stream is pending accept
    pub is_pending_send: bool,

    /// Next node in the linked list of streams waiting for additional
    /// connection level capacity.
    pub next_pending_send_capacity: Option<store::Key>,

    /// True if the stream is waiting for outbound connection capacity
    pub is_pending_send_capacity: bool,

    /// Next node in the open linked list
    pub next_open: Option<store::Key>,

    /// Set to true when the stream is queued in the pending-open list.
    pub is_queued_open: bool,

    // ===== Fields related to receiving =====
    /// Next node in the accept linked list
    pub next_pending_accept: Option<store::Key>,

    /// Set to true when the stream is pending accept
    pub is_pending_accept: bool,

    /// Next node in the linked list of streams waiting to send window updates.
    pub next_window_update: Option<store::Key>,

    /// True if the stream is waiting to send a window update
    pub is_pending_window_update: bool,

    /// The time when this stream may have been locally reset.
    pub reset_at: Option<Instant>,

    /// Next node in list of reset streams that should expire eventually
    pub next_reset_expire: Option<store::Key>,

    /// Validate content-length headers
    pub content_length: ContentLength,
}

/// State related to validating a stream's content-length
#[derive(Debug)]
pub enum ContentLength {
    Omitted,
    Head,
    Remaining(u64),
}

#[derive(Debug)]
pub(super) struct NextAccept;

#[derive(Debug)]
pub(super) struct NextSend;

/// Used for the "operation" queue
#[derive(Debug)]
pub(super) struct NextSendCapacity;

#[derive(Debug)]
pub(super) struct NextWindowUpdate;

#[derive(Debug)]
pub(super) struct NextOpen;

#[derive(Debug)]
pub(super) struct NextResetExpire;

impl Stream {
    pub fn new(id: StreamId, init_send_window: WindowSize, init_recv_window: WindowSize) -> Stream {
        let shared_state = Shared::new(id, init_send_window, init_recv_window);
        Stream::new_with_shared(shared_state)
    }

    pub fn new_with_shared(shared_state: Arc<Shared>) -> Stream {
        let id = shared_state.id;
        Stream {
            id,
            shared_state,
            inner: Inner {
                is_counted: false,

                // ===== Fields related to sending =====
                next_pending_send: None,
                is_pending_send: false,
                is_pending_send_capacity: false,
                next_pending_send_capacity: None,
                is_queued_open: false,
                next_open: None,

                // ===== Fields related to receiving =====
                next_pending_accept: None,
                is_pending_accept: false,
                next_window_update: None,
                is_pending_window_update: false,
                reset_at: None,
                next_reset_expire: None,
                content_length: ContentLength::Omitted,
            },
        }
    }

    pub fn shared_ref(&self) -> &Shared {
        self.shared_state.as_ref()
    }

    pub fn clone_shared(&self) -> Arc<Shared> {
        self.shared_state.clone()
    }

    pub fn shared(&self) -> MutexGuard<'_, SharedState> {
        self.shared_state.state()
    }

    pub fn send(&self) -> MutexGuard<'_, Send> {
        self.shared_state.send()
    }

    pub fn recv(&self) -> MutexGuard<'_, Recv> {
        self.shared_state.recv()
    }

    pub fn inner(&self) -> &Inner {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut Inner {
        &mut self.inner
    }

    /// Returns true if stream is currently being held for some time because of
    /// a local reset.
    pub fn is_pending_reset_expiration(&self) -> bool {
        self.inner().is_pending_reset_expiration()
    }

    /// Returns true if frames for this stream are ready to be sent over the wire
    pub fn is_send_ready(&self) -> bool {
        let is_pending_open = self.send().is_pending_open;
        let is_pending_push = self.shared().is_pending_push;
        !is_pending_open && !is_pending_push
    }

    /// Returns true if the stream is closed
    pub fn is_closed(&self) -> bool {
        self.shared_state.is_closed()
    }

    /// Returns true if the stream is no longer in use
    pub fn is_released(&self) -> bool {
        let (ref_count, pending_operation_count, is_closed) = {
            let shared = self.shared();
            let send = self.send();
            (
                shared.ref_count,
                shared.pending_operation_count,
                shared.state.is_closed()
                    && send.pending_send.is_empty()
                    && send.pending_op_data.is_empty()
                    && send.buffered_send_data == 0,
            )
        };

        let inner = self.inner();
        is_closed
            && ref_count == 0
            && pending_operation_count == 0
            && !inner.is_pending_send
            && !inner.is_pending_send_capacity
            && !inner.is_pending_accept
            && !inner.is_pending_window_update
            && !inner.is_queued_open
            && inner.reset_at.is_none()
    }

    /// Returns true when the consumer of the stream has dropped all handles
    /// (indicating no further interest in the stream) and the stream state is
    /// not actually closed.
    ///
    /// In this case, a reset should be sent.
    pub fn is_canceled_interest(&self) -> bool {
        self.shared_state.is_canceled_interest()
    }

    pub fn assign_capacity(&self, capacity: WindowSize, max_buffer_size: usize) {
        self.shared_state.assign_capacity(capacity, max_buffer_size);
    }

    pub fn send_data(&self, len: WindowSize, max_buffer_size: usize) {
        self.shared_state.send_data(len, max_buffer_size);
    }

    /// If the capacity was limited because of the max_send_buffer_size,
    /// then consider waking the send task again...
    /// Returns `Err` when the decrement cannot be completed due to overflow.
    pub fn dec_content_length(&mut self, len: usize) -> Result<(), ()> {
        self.inner_mut().dec_content_length(len)
    }

    pub fn ensure_content_length_zero(&self) -> Result<(), ()> {
        self.inner().ensure_content_length_zero()
    }

    pub fn notify_send(&self) {
        self.shared_state.notify_send();
    }

    pub fn notify_recv(&self) {
        self.shared_state.notify_recv();
    }

    pub(super) fn notify_push(&self) {
        self.shared_state.notify_push();
    }

    /// Set the stream's state to `Closed` with the given reason and initiator.
    /// Notify the send, receive, and push tasks, if they exist.
    pub(super) fn set_reset(&self, reason: Reason, initiator: Initiator) {
        self.shared_state.set_reset(reason, initiator);
    }
}

impl fmt::Debug for Stream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.inner();
        let state = self.shared();
        let send = self.send();
        let recv = self.recv();
        f.debug_struct("Stream")
            .field("id", &self.id)
            .field("state", &state.state)
            .field("is_counted", &inner.is_counted)
            .field("ref_count", &state.ref_count)
            .h2_field_some("next_pending_send", &inner.next_pending_send)
            .h2_field_if("is_pending_send", &inner.is_pending_send)
            .field("send_flow", &send.send_flow)
            .field("requested_send_capacity", &send.requested_send_capacity)
            .field("buffered_send_data", &send.buffered_send_data)
            .h2_field_some("send_task", &send.send_task.as_ref().map(|_| ()))
            .h2_field_if_then(
                "pending_send",
                !send.pending_send.is_empty(),
                &send.pending_send,
            )
            .h2_field_if_then(
                "pending_op_data",
                !send.pending_op_data.is_empty(),
                &send.pending_op_data,
            )
            .h2_field_if_then(
                "pending_operation_count",
                state.pending_operation_count > 0,
                &state.pending_operation_count,
            )
            .h2_field_some(
                "next_pending_send_capacity",
                &inner.next_pending_send_capacity,
            )
            .h2_field_if("is_pending_send_capacity", &inner.is_pending_send_capacity)
            .h2_field_if("send_capacity_inc", &send.send_capacity_inc)
            .h2_field_some("next_open", &inner.next_open)
            .h2_field_if("is_pending_open", &send.is_pending_open)
            .h2_field_if("is_pending_push", &state.is_pending_push)
            .h2_field_some("next_pending_accept", &inner.next_pending_accept)
            .h2_field_if("is_pending_accept", &inner.is_pending_accept)
            .field("recv_flow", &recv.recv_flow)
            .field("in_flight_recv_data", &recv.in_flight_recv_data)
            .h2_field_some("next_window_update", &inner.next_window_update)
            .h2_field_if("is_pending_window_update", &inner.is_pending_window_update)
            .h2_field_some("reset_at", &inner.reset_at)
            .h2_field_some("next_reset_expire", &inner.next_reset_expire)
            .h2_field_if_then(
                "pending_recv",
                !recv.pending_recv.is_empty(),
                &recv.pending_recv,
            )
            .h2_field_if("is_recv", &recv.is_recv)
            .h2_field_some("recv_task", &recv.recv_task.as_ref().map(|_| ()))
            .h2_field_some("push_task", &recv.push_task.as_ref().map(|_| ()))
            .h2_field_if_then(
                "pending_push_promises",
                !recv.pending_push_promises.is_empty(),
                &recv.pending_push_promises,
            )
            .field("content_length", &inner.content_length)
            .finish()
    }
}

impl store::Next for NextAccept {
    fn next(stream: &Inner) -> Option<store::Key> {
        stream.next_pending_accept
    }

    fn set_next(stream: &mut Inner, key: Option<store::Key>) {
        stream.next_pending_accept = key;
    }

    fn take_next(stream: &mut Inner) -> Option<store::Key> {
        stream.next_pending_accept.take()
    }

    fn is_queued(stream: &Inner) -> bool {
        stream.is_pending_accept
    }

    fn set_queued(stream: &mut Inner, val: bool) {
        stream.is_pending_accept = val;
    }
}

impl store::Next for NextSend {
    fn next(stream: &Inner) -> Option<store::Key> {
        stream.next_pending_send
    }

    fn set_next(stream: &mut Inner, key: Option<store::Key>) {
        stream.next_pending_send = key;
    }

    fn take_next(stream: &mut Inner) -> Option<store::Key> {
        stream.next_pending_send.take()
    }

    fn is_queued(stream: &Inner) -> bool {
        stream.is_pending_send
    }

    fn set_queued(stream: &mut Inner, val: bool) {
        stream.is_pending_send = val;
    }
}

impl store::Next for NextSendCapacity {
    fn next(stream: &Inner) -> Option<store::Key> {
        stream.next_pending_send_capacity
    }

    fn set_next(stream: &mut Inner, key: Option<store::Key>) {
        stream.next_pending_send_capacity = key;
    }

    fn take_next(stream: &mut Inner) -> Option<store::Key> {
        stream.next_pending_send_capacity.take()
    }

    fn is_queued(stream: &Inner) -> bool {
        stream.is_pending_send_capacity
    }

    fn set_queued(stream: &mut Inner, val: bool) {
        stream.is_pending_send_capacity = val;
    }
}

impl store::Next for NextWindowUpdate {
    fn next(stream: &Inner) -> Option<store::Key> {
        stream.next_window_update
    }

    fn set_next(stream: &mut Inner, key: Option<store::Key>) {
        stream.next_window_update = key;
    }

    fn take_next(stream: &mut Inner) -> Option<store::Key> {
        stream.next_window_update.take()
    }

    fn is_queued(stream: &Inner) -> bool {
        stream.is_pending_window_update
    }

    fn set_queued(stream: &mut Inner, val: bool) {
        stream.is_pending_window_update = val;
    }
}

impl store::Next for NextOpen {
    fn next(stream: &Inner) -> Option<store::Key> {
        stream.next_open
    }

    fn set_next(stream: &mut Inner, key: Option<store::Key>) {
        stream.next_open = key;
    }

    fn take_next(stream: &mut Inner) -> Option<store::Key> {
        stream.next_open.take()
    }

    fn is_queued(stream: &Inner) -> bool {
        stream.is_queued_open
    }

    fn set_queued(stream: &mut Inner, val: bool) {
        stream.is_queued_open = val;
    }
}

impl store::Next for NextResetExpire {
    fn next(stream: &Inner) -> Option<store::Key> {
        stream.next_reset_expire
    }

    fn set_next(stream: &mut Inner, key: Option<store::Key>) {
        stream.next_reset_expire = key;
    }

    fn take_next(stream: &mut Inner) -> Option<store::Key> {
        stream.next_reset_expire.take()
    }

    fn is_queued(stream: &Inner) -> bool {
        stream.reset_at.is_some()
    }

    fn set_queued(stream: &mut Inner, val: bool) {
        if val {
            stream.reset_at = Some(Instant::now());
        } else {
            stream.reset_at = None;
        }
    }
}

// ===== impl ContentLength =====

impl ContentLength {
    pub fn is_head(&self) -> bool {
        matches!(*self, Self::Head)
    }
}

impl Shared {
    pub fn new(
        id: StreamId,
        init_send_window: WindowSize,
        init_recv_window: WindowSize,
    ) -> Arc<Self> {
        let mut send_flow = FlowControl::new();
        let mut recv_flow = FlowControl::new();

        recv_flow
            .inc_window(init_recv_window)
            .expect("invalid initial receive window");
        let _res = recv_flow.assign_capacity(init_recv_window);
        debug_assert!(_res.is_ok());

        send_flow
            .inc_window(init_send_window)
            .expect("invalid initial send window size");

        Arc::new(Shared {
            id,
            store: AtomicU32::new(u32::MAX),
            state: Mutex::new(SharedState {
                state: State::default(),
                ref_count: 0,
                pending_operation_count: 0,
                is_pending_push: false,
            }),
            send: Mutex::new(Send {
                // ===== Fields related to sending =====
                send_flow,
                requested_send_capacity: 0,
                buffered_send_data: 0,
                send_task: None,
                pending_send: buffer::Deque::new(),
                pending_op_data: buffer::Deque::new(),
                send_capacity_inc: false,
                is_pending_open: false,
            }),
            recv: Mutex::new(Recv {
                // ===== Fields related to receiving =====
                recv_flow,
                in_flight_recv_data: 0,
                pending_recv: buffer::Deque::new(),
                is_recv: true,
                recv_task: None,
                push_task: None,
                pending_push_promises: buffer::Deque::new(),
            }),
        })
    }

    pub fn store(&self) -> Option<store::Key> {
        let idx = self.store.load(Ordering::Relaxed);
        store::Key::from_parts(self.id, idx)
    }

    pub fn stored(&self, idx: u32) {
        self.store.store(idx, Ordering::Relaxed);
    }

    pub fn state(&self) -> MutexGuard<'_, SharedState> {
        self.state.lock().unwrap()
    }

    pub fn send(&self) -> MutexGuard<'_, Send> {
        self.send.lock().unwrap()
    }

    pub fn recv(&self) -> MutexGuard<'_, Recv> {
        self.recv.lock().unwrap()
    }

    /// Increment the stream's ref count
    pub fn ref_inc(&self) {
        let mut shared = self.state();
        assert!(shared.ref_count < usize::MAX);
        shared.ref_count += 1;
    }

    /// Decrements the stream's ref count
    pub fn ref_dec(&self) {
        let mut shared = self.state();
        assert!(shared.ref_count > 0);
        shared.ref_count -= 1;
    }

    /// Increment the queued operation count for this stream.
    pub fn ops_inc(&self) {
        let mut shared = self.state();
        assert!(shared.pending_operation_count < usize::MAX);
        shared.pending_operation_count += 1;
    }

    /// Decrement the queued operation count for this stream.
    pub fn ops_dec(&self) {
        let mut shared = self.state();
        assert!(shared.pending_operation_count > 0);
        shared.pending_operation_count -= 1;
    }

    pub fn notify_send(&self) {
        self.send().notify_send();
    }

    pub fn wait_send(&self, cx: &Context) {
        self.send().wait_send(cx);
    }

    pub fn notify_recv(&self) {
        self.recv().notify_recv();
    }

    pub fn notify_push(&self) {
        self.recv().notify_push();
    }

    pub fn set_reset(&self, reason: Reason, initiator: Initiator) {
        self.state().state.set_reset(self.id, reason, initiator);
        self.notify_send();
        self.notify_push();
        self.notify_recv();
    }

    pub fn handle_error(&self, err: &proto::Error) {
        self.state().state.handle_error(err);
        self.notify_send();
        self.notify_push();
        self.notify_recv();
    }

    pub fn is_pending_open(&self) -> bool {
        self.send().is_pending_open
    }

    fn is_closed(&self) -> bool {
        let state = self.state();
        let send = self.send();
        state.state.is_closed()
            && send.pending_send.is_empty()
            && send.pending_op_data.is_empty()
            && send.buffered_send_data == 0
    }

    fn is_canceled_interest(&self) -> bool {
        let shared = self.state();
        shared.ref_count == 0 && !shared.state.is_closed()
    }

    pub fn capacity(&self, max_buffer_size: usize) -> WindowSize {
        let shared = self.send();
        let available = shared.send_flow.available().as_size() as usize;
        available
            .min(max_buffer_size)
            .saturating_sub(shared.buffered_send_data) as WindowSize
    }

    fn assign_capacity(&self, capacity: WindowSize, max_buffer_size: usize) {
        let mut shared = self.send();
        let prev_capacity = shared.capacity(max_buffer_size);
        debug_assert!(capacity > 0);
        let _res = shared.send_flow.assign_capacity(capacity);
        debug_assert!(_res.is_ok());

        tracing::trace!(
            "  assigned capacity to stream; available={}; buffered={}; id={:?}; max_buffer_size={} prev={}",
            shared.send_flow.available(),
            shared.buffered_send_data,
            self.id,
            max_buffer_size,
            prev_capacity,
        );

        if prev_capacity < shared.capacity(max_buffer_size) {
            shared.notify_capacity();
        }
    }

    fn send_data(&self, len: WindowSize, max_buffer_size: usize) {
        let mut shared = self.send();
        let prev_capacity = shared.capacity(max_buffer_size);
        let _res = shared.send_flow.send_data(len);
        debug_assert!(_res.is_ok());

        debug_assert!(shared.buffered_send_data >= len as usize);
        shared.buffered_send_data -= len as usize;
        shared.requested_send_capacity -= len;

        tracing::trace!(
            "  sent stream data; available={}; buffered={}; id={:?}; max_buffer_size={} prev={}",
            shared.send_flow.available(),
            shared.buffered_send_data,
            self.id,
            max_buffer_size,
            prev_capacity,
        );

        if prev_capacity < shared.capacity(max_buffer_size) {
            shared.notify_capacity();
        }
    }
}

impl Send {
    pub(super) fn capacity(&self, max_buffer_size: usize) -> WindowSize {
        let available = self.send_flow.available().as_size() as usize;
        available
            .min(max_buffer_size)
            .saturating_sub(self.buffered_send_data) as WindowSize
    }

    fn notify_capacity(&mut self) {
        self.send_capacity_inc = true;
        tracing::trace!("  notifying task");
        self.notify_send();
    }

    fn notify_send(&mut self) {
        if let Some(task) = self.send_task.take() {
            task.wake();
        }
    }

    pub fn wait_send(&mut self, cx: &Context) {
        self.send_task = Some(cx.waker().clone());
    }
}

impl Recv {
    fn notify_recv(&mut self) {
        if let Some(task) = self.recv_task.take() {
            task.wake();
        }
    }

    fn notify_push(&mut self) {
        if let Some(task) = self.push_task.take() {
            task.wake();
        }
    }
}

impl Inner {
    fn is_pending_reset_expiration(&self) -> bool {
        self.reset_at.is_some()
    }

    fn dec_content_length(&mut self, len: usize) -> Result<(), ()> {
        match self.content_length {
            ContentLength::Remaining(ref mut rem) => match rem.checked_sub(len as u64) {
                Some(val) => *rem = val,
                None => return Err(()),
            },
            ContentLength::Head => {
                if len != 0 {
                    return Err(());
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn ensure_content_length_zero(&self) -> Result<(), ()> {
        match self.content_length {
            ContentLength::Remaining(0) => Ok(()),
            ContentLength::Remaining(_) => Err(()),
            _ => Ok(()),
        }
    }
}
