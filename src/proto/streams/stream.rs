use super::*;

#[derive(Debug)]
pub(super) struct Stream<B> {
    /// The h2 stream identifier
    pub id: StreamId,

    /// Current state of the stream
    pub state: State,

    // ===== Fields related to sending =====

    /// Next node in the accept linked list
    pub next_pending_send: Option<store::Key>,

    /// Set to true when the stream is pending accept
    pub is_pending_send: bool,

    /// Send data flow control
    pub send_flow: FlowControl,

    /// Amount of send capacity that has been requested, but not yet allocated.
    pub requested_send_capacity: WindowSize,

    /// Amount of data buffered at the prioritization layer.
    pub buffered_send_data: WindowSize,

    /// Task tracking additional send capacity (i.e. window updates).
    pub send_task: Option<task::Task>,

    /// Frames pending for this stream being sent to the socket
    pub pending_send: buffer::Deque<B>,

    /// Next node in the linked list of streams waiting for additional
    /// connection level capacity.
    pub next_pending_send_capacity: Option<store::Key>,

    /// True if the stream is waiting for outbound connection capacity
    pub is_pending_send_capacity: bool,

    /// Set to true when the send capacity has been incremented
    pub send_capacity_inc: bool,

    // ===== Fields related to receiving =====

    /// Next node in the accept linked list
    pub next_pending_accept: Option<store::Key>,

    /// Set to true when the stream is pending accept
    pub is_pending_accept: bool,

    /// Receive data flow control
    pub recv_flow: FlowControl,

    pub in_flight_recv_data: WindowSize,

    /// Next node in the linked list of streams waiting to send window updates.
    pub next_window_update: Option<store::Key>,

    /// True if the stream is waiting to send a window update
    pub is_pending_window_update: bool,

    /// Frames pending for this stream to read
    pub pending_recv: buffer::Deque<Bytes>,

    /// Task tracking receiving frames
    pub recv_task: Option<task::Task>,

    /// The stream's pending push promises
    pub pending_push_promises: store::Queue<B, NextAccept>,
}

#[derive(Debug)]
pub(super) struct NextAccept;

#[derive(Debug)]
pub(super) struct NextSend;

#[derive(Debug)]
pub(super) struct NextSendCapacity;

#[derive(Debug)]
pub(super) struct NextWindowUpdate;

impl<B> Stream<B> {
    pub fn new(id: StreamId) -> Stream<B>
    {
        Stream {
            id,
            state: State::default(),

            // ===== Fields related to sending =====

            next_pending_send: None,
            is_pending_send: false,
            send_flow: FlowControl::new(),
            requested_send_capacity: 0,
            buffered_send_data: 0,
            send_task: None,
            pending_send: buffer::Deque::new(),
            is_pending_send_capacity: false,
            next_pending_send_capacity: None,
            send_capacity_inc: false,

            // ===== Fields related to receiving =====

            next_pending_accept: None,
            is_pending_accept: false,
            recv_flow: FlowControl::new(),
            in_flight_recv_data: 0,
            next_window_update: None,
            is_pending_window_update: false,
            pending_recv: buffer::Deque::new(),
            recv_task: None,
            pending_push_promises: store::Queue::new(),
        }
    }

    pub fn assign_capacity(&mut self, capacity: WindowSize) {
        debug_assert!(capacity > 0);
        self.send_capacity_inc = true;
        self.send_flow.assign_capacity(capacity);

        // Only notify if the capacity exceeds the amount of buffered data
        if self.send_flow.available() > self.buffered_send_data {
            self.notify_send();
        }
    }

    pub fn notify_send(&mut self) {
        if let Some(task) = self.send_task.take() {
            task.notify();
        }
    }

    pub fn notify_recv(&mut self) {
        if let Some(task) = self.recv_task.take() {
            task.notify();
        }
    }
}

impl store::Next for NextAccept {
    fn next<B>(stream: &Stream<B>) -> Option<store::Key> {
        stream.next_pending_accept
    }

    fn set_next<B>(stream: &mut Stream<B>, key: Option<store::Key>) {
        stream.next_pending_accept = key;
    }

    fn take_next<B>(stream: &mut Stream<B>) -> Option<store::Key> {
        stream.next_pending_accept.take()
    }

    fn is_queued<B>(stream: &Stream<B>) -> bool {
        stream.is_pending_accept
    }

    fn set_queued<B>(stream: &mut Stream<B>, val: bool) {
        stream.is_pending_accept = val;
    }
}

impl store::Next for NextSend {
    fn next<B>(stream: &Stream<B>) -> Option<store::Key> {
        stream.next_pending_send
    }

    fn set_next<B>(stream: &mut Stream<B>, key: Option<store::Key>) {
        stream.next_pending_send = key;
    }

    fn take_next<B>(stream: &mut Stream<B>) -> Option<store::Key> {
        stream.next_pending_send.take()
    }

    fn is_queued<B>(stream: &Stream<B>) -> bool {
        stream.is_pending_send
    }

    fn set_queued<B>(stream: &mut Stream<B>, val: bool) {
        stream.is_pending_send = val;
    }
}

impl store::Next for NextSendCapacity {
    fn next<B>(stream: &Stream<B>) -> Option<store::Key> {
        stream.next_pending_send_capacity
    }

    fn set_next<B>(stream: &mut Stream<B>, key: Option<store::Key>) {
        stream.next_pending_send_capacity = key;
    }

    fn take_next<B>(stream: &mut Stream<B>) -> Option<store::Key> {
        stream.next_pending_send_capacity.take()
    }

    fn is_queued<B>(stream: &Stream<B>) -> bool {
        stream.is_pending_send_capacity
    }

    fn set_queued<B>(stream: &mut Stream<B>, val: bool) {
        stream.is_pending_send_capacity = val;
    }
}

impl store::Next for NextWindowUpdate {
    fn next<B>(stream: &Stream<B>) -> Option<store::Key> {
        stream.next_window_update
    }

    fn set_next<B>(stream: &mut Stream<B>, key: Option<store::Key>) {
        stream.next_window_update = key;
    }

    fn take_next<B>(stream: &mut Stream<B>) -> Option<store::Key> {
        stream.next_window_update.take()
    }

    fn is_queued<B>(stream: &Stream<B>) -> bool {
        stream.is_pending_window_update
    }

    fn set_queued<B>(stream: &mut Stream<B>, val: bool) {
        stream.is_pending_window_update = val;
    }
}
